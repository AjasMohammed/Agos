use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;

/// Maximum file size that file-reader will load into memory (10 MiB).
const MAX_FILE_READ_BYTES: u64 = 10 * 1024 * 1024;

/// Identify container formats by magic bytes, returning a MIME for the
/// extractor.
///
/// Extension and MIME are both unavailable here — `file-reader` reads workspace
/// paths, which frequently have neither — and "does it parse as UTF-8" does not
/// separate a text file from a PDF: an uncompressed PDF is pure ASCII.
async fn sniff_container(path: &std::path::Path) -> Option<&'static str> {
    use tokio::io::AsyncReadExt;
    let mut head = [0u8; 8];
    let mut f = tokio::fs::File::open(path).await.ok()?;
    let n = f.read(&mut head).await.ok()?;
    let head = &head[..n];
    if head.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    // ZIP magic covers docx/xlsx/pptx/odt/ods/odp. Left to the extractor's
    // extension check to pick a filter, so a plain `.zip` is not routed to
    // LibreOffice by its magic alone.
    None
}

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

        tracing::debug!(path = path_str, mode, "file-reader: starting");

        // SECURITY: relative paths resolve under the agent's own home, never the
        // kernel state dir (audit.db, api_keys.db, chat.db, agents.json live there).
        let agent_root = context.agent_files_dir()?;
        // SECURITY: resolve path, checking workspace paths before falling back to data_dir.
        let resolved =
            crate::traits::resolve_tool_path(path_str, &agent_root, &context.read_roots())
                .map_err(|e| context.with_path_hint(e))?;

        // Canonicalize to verify containment. For directories that don't exist yet
        // we fall through to a clear error; for existing paths this enforces the boundary.
        let canonical = resolved
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("Path not found: {} ({})", path_str, e),
            })?;

        // Canonicalize data_dir too so the starts_with comparison is apples-to-apples
        // even when data_dir itself contains symlinks (e.g. /tmp on macOS).
        let canonical_agent_root =
            agent_root
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-reader".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        let in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| canonical.starts_with(wp));
        // KMC Phase 3: check dynamic storage zones
        let in_storage_zone = context
            .storage_zone_query
            .as_ref()
            .map(|q| q.is_path_in_zone(&context.agent_id, &canonical))
            .unwrap_or(false);
        if !canonical.starts_with(&canonical_agent_root) && !in_workspace && !in_storage_zone {
            tracing::warn!(path = path_str, "file-reader: path traversal blocked");
            return Err(context.deny_path(path_str));
        }
        if in_workspace
            && !context
                .permissions
                .check("fs.workspace", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.workspace".into(),
                operation: format!("Workspace read access denied: {}", path_str),
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

        // Size guard: reject files larger than 10 MiB to prevent OOM.
        let metadata = tokio::fs::metadata(&canonical).await.map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("Cannot stat {}: {}", path_str, e),
            }
        })?;
        let size_bytes = metadata.len();
        if size_bytes > MAX_FILE_READ_BYTES {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!(
                    "File too large: {} bytes (limit {} bytes). Use pagination or a stream tool.",
                    size_bytes, MAX_FILE_READ_BYTES
                ),
            });
        }

        // Non-UTF-8 on disk is not necessarily unreadable: a workspace PDF or
        // Office document converts to text just like an uploaded one.
        //
        // Sniffed first, because "is it valid UTF-8" is the wrong question for a
        // container format. A PDF with uncompressed content streams is entirely
        // ASCII, so `read_to_string` succeeds and hands the agent
        // `%PDF-1.4 1 0 obj << /Type /Catalog …` — the exact dead end this
        // module exists to remove.
        let sniffed = match sniff_container(&canonical).await {
            Some(mime) => crate::extract::read_as_text(&canonical, mime).await,
            None => None,
        };
        let (content, extracted) = match sniffed {
            Some(text) => (text, true),
            // Falls through rather than erroring: the sniff is a five-byte magic
            // check, so a text file that merely *starts* with `%PDF-` would
            // otherwise become a hard error on a file that reads fine. A real
            // PDF with no text layer lands here too and yields its raw bytes,
            // which is what this path did before the sniff existed.
            None => match tokio::fs::read_to_string(&canonical).await {
                Ok(text) => (text, false),
                // Only non-UTF-8 is worth a conversion attempt; a permissions
                // error fails again inside the extractor after burning a
                // converter timeout.
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    match crate::extract::read_as_text(&canonical, "").await {
                        Some(text) => (text, true),
                        None => {
                            return Err(AgentOSError::ToolExecutionFailed {
                                tool_name: "file-reader".into(),
                                reason: format!("Cannot read {}: {}", path_str, e),
                            })
                        }
                    }
                }
                Err(e) => {
                    return Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "file-reader".into(),
                        reason: format!("Cannot read {}: {}", path_str, e),
                    })
                }
            },
        };

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

        tracing::debug!(
            path = path_str,
            size_bytes,
            total_lines,
            returned_lines,
            "file-reader: read complete"
        );

        Ok(serde_json::json!({
            "path": path_str,
            "content": page_content,
            "size_bytes": size_bytes,
            "total_lines": total_lines,
            "returned_lines": returned_lines,
            "offset": offset,
            "has_more": has_more,
            "content_type": "text",
            "extracted": extracted,
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
