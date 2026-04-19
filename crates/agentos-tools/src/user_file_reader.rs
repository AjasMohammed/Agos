use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use rusqlite::{params, Connection};

/// 50 MiB read cap for agent-initiated file reads.
const MAX_READ_BYTES: u64 = 50 * 1024 * 1024;

pub struct UserFileReader;

impl UserFileReader {
    pub fn new() -> Self {
        Self
    }
}

impl Default for UserFileReader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for UserFileReader {
    fn name(&self) -> &str {
        "user-file-reader"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let file_id = payload.get("file_id").and_then(|v| v.as_str());
        let file_name = payload.get("file_name").and_then(|v| v.as_str());

        if file_id.is_none() && file_name.is_none() {
            return Err(AgentOSError::SchemaValidation(
                "user-file-reader requires 'file_id' or 'file_name'".into(),
            ));
        }

        let uploads_dir = context.data_dir.join("uploads");
        let db_path = uploads_dir.join("file_registry.db");

        if !db_path.exists() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "user-file-reader".into(),
                reason: "File registry not found — no files have been uploaded yet".into(),
            });
        }

        let fid = file_id.map(String::from);
        let fname = file_name.map(String::from);

        // Open a short-lived connection; the registry is created/managed by the web server.
        let (record_path, record_name, record_mime, record_size) = tokio::task::spawn_blocking(
            move || -> Result<(String, String, String, u64), String> {
                let conn = Connection::open(&db_path).map_err(|e| format!("open registry: {e}"))?;

                let row: Option<(String, String, String, i64)> = if let Some(id) = &fid {
                    conn.query_row(
                        "SELECT path, original_name, mime, size FROM uploaded_files WHERE id = ?1",
                        params![id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .ok()
                } else if let Some(name) = &fname {
                    conn.query_row(
                        "SELECT path, original_name, mime, size FROM uploaded_files
                         WHERE name = ?1 OR original_name = ?1
                         ORDER BY uploaded_at DESC LIMIT 1",
                        params![name],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .ok()
                } else {
                    None
                };

                let (path, name, mime, size) =
                    row.ok_or_else(|| "File not found in registry".to_string())?;
                Ok((path, name, mime, size as u64))
            },
        )
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "user-file-reader".into(),
            reason: format!("spawn_blocking panicked: {e}"),
        })?
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "user-file-reader".into(),
            reason: e,
        })?;

        // SECURITY: verify stored path is inside the uploads directory.
        let canonical_uploads =
            uploads_dir
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "user-file-reader".into(),
                    reason: format!("Cannot resolve uploads dir: {e}"),
                })?;

        let disk_path = std::path::PathBuf::from(&record_path);
        let canonical_path =
            disk_path
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "user-file-reader".into(),
                    reason: format!("File not found on disk: {e}"),
                })?;

        if !canonical_path.starts_with(&canonical_uploads) {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "user-file-reader".into(),
                reason: "File path is outside the uploads directory".into(),
            });
        }

        if record_size > MAX_READ_BYTES {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "user-file-reader".into(),
                reason: format!(
                    "File too large to read ({} MiB, max 50 MiB)",
                    record_size / (1024 * 1024)
                ),
            });
        }

        // Decide whether to return text or base64 based on mime type.
        let is_text = record_mime.starts_with("text/")
            || record_mime.contains("json")
            || record_mime.contains("xml")
            || record_mime.contains("javascript")
            || record_mime.contains("yaml")
            || record_mime.contains("toml")
            || record_mime.contains("markdown");

        if is_text {
            let content = tokio::fs::read_to_string(&canonical_path)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "user-file-reader".into(),
                    reason: format!("Failed to read file: {e}"),
                })?;

            Ok(serde_json::json!({
                "filename": record_name,
                "mime":     record_mime,
                "size":     record_size,
                "encoding": "text",
                "content":  content,
            }))
        } else {
            let bytes = tokio::fs::read(&canonical_path).await.map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "user-file-reader".into(),
                    reason: format!("Failed to read file: {e}"),
                }
            })?;

            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

            Ok(serde_json::json!({
                "filename": record_name,
                "mime":     record_mime,
                "size":     record_size,
                "encoding": "base64",
                "content":  b64,
            }))
        }
    }
}
