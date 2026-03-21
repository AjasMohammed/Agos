use crate::file_lock::WriteLockGuard;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

pub struct FileMove;

impl FileMove {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileMove {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for FileMove {
    fn name(&self) -> &str {
        "file-move"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let from_str = payload
            .get("from")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("file-move requires 'from' field".into())
            })?;

        let to_str = payload.get("to").and_then(|v| v.as_str()).ok_or_else(|| {
            AgentOSError::SchemaValidation("file-move requires 'to' field".into())
        })?;

        tracing::debug!(from = from_str, to = to_str, "file-move: starting");

        let canonical_data_dir =
            context
                .data_dir
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-move".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        // SECURITY: resolve source, checking workspace paths first.
        let from_resolved =
            crate::traits::resolve_tool_path(from_str, &context.data_dir, &context.workspace_paths);
        let canonical_from =
            from_resolved
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-move".into(),
                    reason: format!("Source not found: {} ({})", from_str, e),
                })?;

        let from_in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| canonical_from.starts_with(wp));
        if !canonical_from.starts_with(&canonical_data_dir) && !from_in_workspace {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied (from): {}", from_str),
            });
        }
        if from_in_workspace
            && !context
                .permissions
                .check("fs.workspace", PermissionOp::Write)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.workspace".into(),
                operation: format!("Workspace write access denied (from): {}", from_str),
            });
        }

        // SECURITY: resolve destination, checking workspace paths first.
        // The destination may not exist yet → use lexical normalize_path.
        let to_resolved =
            crate::traits::resolve_tool_path(to_str, &context.data_dir, &context.workspace_paths);
        let normalized_to = normalize_path(&to_resolved);

        let to_in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| normalized_to.starts_with(wp));
        if !normalized_to.starts_with(&canonical_data_dir) && !to_in_workspace {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied (to): {}", to_str),
            });
        }
        if to_in_workspace
            && !context
                .permissions
                .check("fs.workspace", PermissionOp::Write)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.workspace".into(),
                operation: format!("Workspace write access denied (to): {}", to_str),
            });
        }

        if canonical_from == normalized_to {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "file-move".into(),
                reason: "Source and destination are the same path".into(),
            });
        }

        // HIGH-3: Refuse to silently overwrite an existing destination.
        if tokio::fs::metadata(&normalized_to).await.is_ok() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "file-move".into(),
                reason: format!(
                    "Destination already exists: {}. Delete it first with file-delete.",
                    to_str
                ),
            });
        }

        // Acquire write lock on source path.
        let _lock_guard = if let Some(registry) = &context.file_lock_registry {
            Some(WriteLockGuard::acquire(
                registry,
                canonical_from.clone(),
                context.agent_id,
                context.task_id,
            )?)
        } else {
            None
        };

        // Create parent directories for destination.
        if let Some(parent) = normalized_to.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "file-move".into(),
                    reason: format!("Cannot create destination directory: {}", e),
                }
            })?;
        }

        // CRITICAL-1: After creating parent dirs, canonicalize the destination's parent
        // and re-join the filename. This detects any symlinks in the newly created path
        // that could escape data_dir, since lexical normalize_path cannot resolve them.
        let final_dest = if let Some(parent) = normalized_to.parent() {
            let canonical_parent =
                parent
                    .canonicalize()
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "file-move".into(),
                        reason: format!("Cannot resolve destination parent: {}", e),
                    })?;
            let parent_in_workspace = context
                .workspace_paths
                .iter()
                .any(|wp| canonical_parent.starts_with(wp));
            if !canonical_parent.starts_with(&canonical_data_dir) && !parent_in_workspace {
                return Err(AgentOSError::PermissionDenied {
                    resource: "fs.user_data".into(),
                    operation: format!("Path traversal denied (to): {}", to_str),
                });
            }
            canonical_parent.join(normalized_to.file_name().ok_or_else(|| {
                AgentOSError::SchemaValidation("Destination path has no filename".into())
            })?)
        } else {
            normalized_to.clone()
        };

        tokio::fs::rename(&canonical_from, &final_dest)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-move".into(),
                reason: format!("Cannot move {} to {}: {}", from_str, to_str, e),
            })?;

        Ok(serde_json::json!({
            "from": from_str,
            "to": to_str,
            "success": true,
        }))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    crate::workspace::normalize_path(path)
}
