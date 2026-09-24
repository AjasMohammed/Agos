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

        tracing::debug!(path = path_str, "file-delete: starting");

        // SECURITY: relative paths resolve under the agent's own home, never the
        // kernel state dir (audit.db, api_keys.db, chat.db, agents.json live there).
        let agent_root = context.agent_files_dir()?;
        // SECURITY: file-delete writes — resolve against the *writable* workspace list.
        let resolved = crate::traits::resolve_tool_path(
            path_str,
            &agent_root,
            // Writable grants AND read-write storage zones — the conversation's
            // shared workspace is a zone, and the resolver rejects an unknown
            // absolute path before the zone check further down ever runs.
            &context.write_roots(),
        )
        .map_err(|e| context.with_path_hint(e))?;

        // canonicalize verifies the file actually exists and resolves symlinks.
        let canonical = resolved
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-delete".into(),
                reason: format!("Path not found: {} ({})", path_str, e),
            })?;

        let canonical_agent_root =
            agent_root
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-delete".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        let in_workspace = context
            .workspace_paths_writable
            .iter()
            .any(|wp| canonical.starts_with(wp));
        // KMC Phase 3: check dynamic storage zones
        let in_storage_zone = context
            .storage_zone_query
            .as_ref()
            .map(|q| q.is_path_in_zone(&context.agent_id, &canonical))
            .unwrap_or(false);
        if !canonical.starts_with(&canonical_agent_root) && !in_workspace && !in_storage_zone {
            tracing::warn!(path = path_str, "file-delete: path traversal blocked");
            return Err(context.deny_path(path_str));
        }
        // KMC: enforce read-only zones — deny writes to ReadOnly storage zones.
        if in_storage_zone {
            let access = context
                .storage_zone_query
                .as_ref()
                .and_then(|q| q.zone_access(&context.agent_id, &canonical));
            if access == Some(agentos_types::ZoneAccessLevel::ReadOnly) {
                return Err(AgentOSError::PermissionDenied {
                    resource: "fs.user_data".into(),
                    operation: format!("Write denied: storage zone is read-only for {}", path_str),
                });
            }
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

        let recursive = payload
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let permanent = payload
            .get("permanent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let is_dir = tokio::fs::metadata(&canonical)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-delete".into(),
                reason: format!("Cannot stat {}: {}", path_str, e),
            })?
            .is_dir();
        if is_dir {
            if !recursive {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "file-delete".into(),
                    reason: format!(
                        "Path is a directory: {}. Pass recursive=true to delete it and everything inside.",
                        path_str
                    ),
                });
            }
            // SECURITY: never remove a sandbox root (agent home, a granted
            // folder) or anything that contains one, and never another agent's
            // home even when an operator grant happens to cover `data_dir`.
            let agents_dir = context.data_dir.join("agents");
            let agents_dir = agents_dir.canonicalize().unwrap_or(agents_dir);
            let holds_root = std::iter::once(&canonical_agent_root)
                .chain(context.workspace_paths.iter())
                .any(|root| root.starts_with(&canonical))
                || (canonical.parent() == Some(agents_dir.as_path()));
            if holds_root {
                return Err(AgentOSError::PermissionDenied {
                    resource: "fs.user_data".into(),
                    operation: format!(
                        "Refusing to delete {}: it is, or contains, your home dir or a granted folder",
                        path_str
                    ),
                });
            }
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

        // Deleting something already in the trash is how an agent empties it.
        let trash_root = canonical_agent_root.join(crate::workspace::TRASH_DIR);
        let recoverable_at = if permanent || canonical.starts_with(&trash_root) {
            let removed = if is_dir {
                tokio::fs::remove_dir_all(&canonical).await
            } else {
                tokio::fs::remove_file(&canonical).await
            };
            removed.map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-delete".into(),
                reason: format!("Cannot delete {}: {}", path_str, e),
            })?;
            None
        } else {
            Some(crate::workspace::trash("file-delete", &canonical_agent_root, &canonical).await?)
        };

        tracing::debug!(path = path_str, ?recoverable_at, "file-delete: complete");

        let mut out = serde_json::json!({
            "path": path_str,
            "success": true,
            "recoverable": recoverable_at.is_some(),
        });
        if let Some(p) = recoverable_at {
            out["trash_path"] = p.into();
            out["note"] = "Restore with file-move from trash_path. Kept ~72h.".into();
        }
        Ok(out)
    }
}
