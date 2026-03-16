use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use std::path::Path;

pub struct FileReader;

impl FileReader {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileReader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for FileReader {
    fn name(&self) -> &str {
        "file-reader"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let path_str = payload
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("file-reader requires 'path' field".into())
            })?;

        let mode = payload
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("read");

        // SECURITY: resolve path relative to data_dir only. Prevent path traversal.
        let requested_path = Path::new(path_str);
        let resolved = if requested_path.is_absolute() {
            let stripped = requested_path.strip_prefix("/").unwrap_or(requested_path);
            context.data_dir.join(stripped)
        } else {
            context.data_dir.join(requested_path)
        };

        // Canonicalize to verify containment. For directories that don't exist yet
        // we fall through to a clear error; for existing paths this enforces the boundary.
        let canonical = resolved
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("Path not found: {} ({})", path_str, e),
            })?;

        if !canonical.starts_with(&context.data_dir) {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied: {}", path_str),
            });
        }

        // Directory listing — triggered by mode=list OR when the path is a directory.
        let is_dir = canonical.is_dir();
        if mode == "list" || is_dir {
            return list_directory(&canonical, path_str).await;
        }

        // --- File read ---

        // Check the lock registry before reading. A write lock means no reads allowed.
        if let Some(registry) = &context.file_lock_registry {
            registry.check(&canonical)?;
        }

        let content = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("Cannot read {}: {}", path_str, e),
            }
        })?;

        let metadata = tokio::fs::metadata(&canonical).await.ok();
        let size_bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

        // Line-based pagination.
        let offset = payload.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        // limit=0 means no cap; default safety cap is 500 lines.
        let limit_raw = payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(500);
        let limit: Option<usize> = if limit_raw == 0 {
            None
        } else {
            Some(limit_raw as usize)
        };

        let all_lines: Vec<&str> = content.lines().collect();
        let total_lines = all_lines.len();

        let start = offset.min(total_lines);
        let end = match limit {
            Some(n) => (start + n).min(total_lines),
            None => total_lines,
        };
        let has_more = end < total_lines;
        let returned_lines = end - start;

        let page_content = all_lines[start..end].join("\n");

        Ok(serde_json::json!({
            "path": path_str,
            "content": page_content,
            "size_bytes": size_bytes,
            "total_lines": total_lines,
            "returned_lines": returned_lines,
            "offset": offset,
            "has_more": has_more,
            "content_type": "text",
        }))
    }
}

async fn list_directory(
    dir: &std::path::PathBuf,
    original_path: &str,
) -> Result<serde_json::Value, AgentOSError> {
    let mut read_dir =
        tokio::fs::read_dir(dir)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("Cannot list directory {}: {}", original_path, e),
            })?;

    let mut entries = Vec::new();
    while let Some(entry) =
        read_dir
            .next_entry()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("Directory read error: {}", e),
            })?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().await.ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        entries.push(serde_json::json!({
            "name": name,
            "size_bytes": size_bytes,
            "is_dir": is_dir,
        }));
    }

    // Sort by name for deterministic output.
    entries.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });

    let count = entries.len();
    Ok(serde_json::json!({
        "path": original_path,
        "mode": "list",
        "entries": entries,
        "count": count,
    }))
}
