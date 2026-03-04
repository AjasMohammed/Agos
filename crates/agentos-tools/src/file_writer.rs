use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use std::path::{Component, Path, PathBuf};

pub struct FileWriter;

impl FileWriter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileWriter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for FileWriter {
    fn name(&self) -> &str {
        "file-writer"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Write)]
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
                AgentOSError::SchemaValidation("file-writer requires 'path' field".into())
            })?;

        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("file-writer requires 'content' field".into())
            })?;

        let append = payload.get("append").and_then(|v| v.as_bool()).unwrap_or(false);

        // SECURITY: resolve path relative to data_dir only. Prevent path traversal.
        let requested_path = Path::new(path_str);
        let resolved = if requested_path.is_absolute() {
            let stripped = requested_path.strip_prefix("/").unwrap_or(requested_path);
            context.data_dir.join(stripped)
        } else {
            context.data_dir.join(requested_path)
        };

        // Normalize the path to eliminate ".." components BEFORE creating directories.
        // We can't use canonicalize() because the file may not exist yet, so we
        // normalize lexically and check that the result stays within data_dir.
        let normalized = normalize_path(&resolved);
        let canonical_data_dir = context
            .data_dir
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-writer".into(),
                reason: format!("Data directory error: {}", e),
            })?;

        if !normalized.starts_with(&canonical_data_dir) {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied: {}", path_str),
            });
        }

        // Create parent directories if needed (safe now that path is validated)
        if let Some(parent) = normalized.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Cannot create directory: {}", e),
                })?;
        }

        if append {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&normalized)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Cannot open for append: {}", e),
                })?;
            file.write_all(content.as_bytes())
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Write failed: {}", e),
                })?;
        } else {
            tokio::fs::write(&normalized, content)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Write failed: {}", e),
                })?;
        }

        Ok(serde_json::json!({
            "path": path_str,
            "bytes_written": content.len(),
            "mode": if append { "append" } else { "overwrite" },
            "success": true,
        }))
    }
}

/// Lexically normalize a path by resolving `.` and `..` components without touching the filesystem.
/// This is needed because the target file may not exist yet (so canonicalize() would fail).
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {} // skip "."
            other => result.push(other),
        }
    }
    result
}
