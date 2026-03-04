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

        // SECURITY: resolve path relative to data_dir only. Prevent path traversal.
        let requested_path = Path::new(path_str);
        let resolved = if requested_path.is_absolute() {
            // Strip leading / and resolve relative to data_dir
            let stripped = requested_path.strip_prefix("/").unwrap_or(requested_path);
            context.data_dir.join(stripped)
        } else {
            context.data_dir.join(requested_path)
        };

        // Canonicalize and verify it's within data_dir
        let canonical = resolved
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("File not found: {} ({})", path_str, e),
            })?;

        if !canonical.starts_with(&context.data_dir) {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied: {}", path_str),
            });
        }

        // Read the file
        let content = tokio::fs::read_to_string(&canonical)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("Cannot read {}: {}", path_str, e),
            })?;

        let metadata = tokio::fs::metadata(&canonical).await.ok();
        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

        Ok(serde_json::json!({
            "path": path_str,
            "content": content,
            "size_bytes": size,
            "content_type": "text",
        }))
    }
}
