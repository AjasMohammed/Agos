use crate::file_lock::WriteLockGuard;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;

pub struct FileDelete;

impl FileDelete {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileDelete {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for FileDelete {
    fn name(&self) -> &str {
        "file-delete"
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
                AgentOSError::SchemaValidation("file-delete requires 'path' field".into())
            })?;

        // SECURITY: resolve path, checking workspace paths before falling back to data_dir.
        let resolved =
            crate::traits::resolve_tool_path(path_str, &context.data_dir, &context.workspace_paths);

        // canonicalize verifies the file actually exists and resolves symlinks.
        let canonical = resolved
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-delete".into(),
                reason: format!("Path not found: {} ({})", path_str, e),
            })?;

        let canonical_data_dir =
            context
                .data_dir
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-delete".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        let in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| canonical.starts_with(wp));
        if !canonical.starts_with(&canonical_data_dir) && !in_workspace {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied: {}", path_str),
            });
        }
        if in_workspace
            && !context
                .permissions
                .check("fs.workspace", PermissionOp::Write)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.workspace".into(),
                operation: format!("Workspace write access denied: {}", path_str),
            });
        }

        // Reject directories — use a dedicated dir-delete intent, not this tool.
        if canonical.is_dir() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "file-delete".into(),
                reason: format!(
                    "Path is a directory: {}. file-delete only removes files.",
                    path_str
                ),
            });
        }

        // Acquire write lock before deleting.
        let _lock_guard = if let Some(registry) = &context.file_lock_registry {
            Some(WriteLockGuard::acquire(
                registry,
                canonical.clone(),
                context.agent_id,
                context.task_id,
            )?)
        } else {
            None
        };

        tokio::fs::remove_file(&canonical).await.map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "file-delete".into(),
                reason: format!("Cannot delete {}: {}", path_str, e),
            }
        })?;

        Ok(serde_json::json!({
            "path": path_str,
            "success": true,
        }))
    }
}
