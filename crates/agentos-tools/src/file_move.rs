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

        // copy=true leaves the source in place, so it only needs read access.
        let copy = payload
            .get("copy")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (from_roots, from_op) = if copy {
            (&context.workspace_paths, PermissionOp::Read)
        } else {
            (&context.workspace_paths_writable, PermissionOp::Write)
        };
        // Roots the resolver accepts = grants + zones. Kept separate from
        // `from_roots` above, which stays grants-only: it drives the
        // `fs.workspace` permission gate, and a storage zone (the conversation
        // shared workspace among them) is kernel space that gate does not cover.
        let from_resolve_roots = if copy {
            context.read_roots()
        } else {
            context.write_roots()
        };

        tracing::debug!(from = from_str, to = to_str, copy, "file-move: starting");

        // SECURITY: relative paths resolve under the agent's own home, never the
        // kernel state dir (audit.db, api_keys.db, chat.db, agents.json live there).
        let agent_root = context.agent_files_dir()?;
        let canonical_agent_root =
            agent_root
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-move".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        // SECURITY: a move deletes the source — both endpoints need write access.
        let from_resolved =
            crate::traits::resolve_tool_path(from_str, &agent_root, &from_resolve_roots)
                .map_err(|e| context.with_path_hint(e))?;
        let canonical_from =
            from_resolved
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-move".into(),
                    reason: format!("Source not found: {} ({})", from_str, e),
                })?;

        let from_in_workspace = from_roots.iter().any(|wp| canonical_from.starts_with(wp));
        // KMC Phase 3: check dynamic storage zones
        let from_in_storage_zone = context
            .storage_zone_query
            .as_ref()
            .map(|q| q.is_path_in_zone(&context.agent_id, &canonical_from))
            .unwrap_or(false);
        if !canonical_from.starts_with(&canonical_agent_root)
            && !from_in_workspace
            && !from_in_storage_zone
        {
            return Err(context.with_path_hint(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied (from): {}", from_str),
            }));
        }
        if from_in_workspace && !context.permissions.check("fs.workspace", from_op) {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.workspace".into(),
                operation: format!("Workspace write access denied (from): {}", from_str),
            });
        }

        // SECURITY: destination must also be in the writable workspace list.
        // The destination may not exist yet → use lexical normalize_path.
        let to_resolved =
            crate::traits::resolve_tool_path(to_str, &agent_root, &context.write_roots())
                .map_err(|e| context.with_path_hint(e))?;
        let normalized_to = normalize_path(&to_resolved);

        let to_in_workspace = context
            .workspace_paths_writable
            .iter()
            .any(|wp| normalized_to.starts_with(wp));
        // KMC Phase 3: check dynamic storage zones
        let to_in_storage_zone = context
            .storage_zone_query
            .as_ref()
            .map(|q| q.is_path_in_zone(&context.agent_id, &normalized_to))
            .unwrap_or(false);
        if !normalized_to.starts_with(&canonical_agent_root)
            && !to_in_workspace
            && !to_in_storage_zone
        {
            return Err(context.with_path_hint(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied (to): {}", to_str),
            }));
        }
        // KMC: enforce read-only zones — deny writes to ReadOnly storage zones.
        if to_in_storage_zone {
            let access = context
                .storage_zone_query
                .as_ref()
                .and_then(|q| q.zone_access(&context.agent_id, &normalized_to));
            if access == Some(agentos_types::ZoneAccessLevel::ReadOnly) {
                return Err(AgentOSError::PermissionDenied {
                    resource: "fs.user_data".into(),
                    operation: format!("Write denied: storage zone is read-only for {}", to_str),
                });
            }
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
        if tokio::fs::symlink_metadata(&normalized_to).await.is_ok() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "file-move".into(),
                reason: format!(
                    "Destination already exists: {}. Delete it first with file-delete.",
                    to_str
                ),
            });
        }

        // A move takes the write lock on the source. A copy does not mutate it,
        // so like file-reader it only checks nobody else is mid-write — two
        // agents may copy the same read-only file at once.
        let _lock_guard = match &context.file_lock_registry {
            Some(registry) if copy => {
                registry.check(&canonical_from)?;
                None
            }
            Some(registry) => Some(WriteLockGuard::acquire(
                registry,
                canonical_from.clone(),
                context.agent_id,
                context.task_id,
            )?),
            None => None,
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
                .workspace_paths_writable
                .iter()
                .any(|wp| canonical_parent.starts_with(wp));
            // KMC Phase 3: check dynamic storage zones
            let parent_in_storage_zone = context
                .storage_zone_query
                .as_ref()
                .map(|q| q.is_path_in_zone(&context.agent_id, &canonical_parent))
                .unwrap_or(false);
            if !canonical_parent.starts_with(&canonical_agent_root)
                && !parent_in_workspace
                && !parent_in_storage_zone
            {
                return Err(context.with_path_hint(AgentOSError::PermissionDenied {
                    resource: "fs.user_data".into(),
                    operation: format!("Path traversal denied (to): {}", to_str),
                }));
            }
            // KMC: enforce read-only zones — deny writes to ReadOnly storage zones.
            if parent_in_storage_zone {
                let access = context
                    .storage_zone_query
                    .as_ref()
                    .and_then(|q| q.zone_access(&context.agent_id, &canonical_parent));
                if access == Some(agentos_types::ZoneAccessLevel::ReadOnly) {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "fs.user_data".into(),
                        operation: format!(
                            "Write denied: storage zone is read-only for {}",
                            to_str
                        ),
                    });
                }
            }
            canonical_parent.join(normalized_to.file_name().ok_or_else(|| {
                AgentOSError::SchemaValidation("Destination path has no filename".into())
            })?)
        } else {
            normalized_to.clone()
        };

        if copy {
            crate::workspace::copy_excl(&canonical_from, &final_dest)
                .await
                .map_err(|reason| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-move".into(),
                    reason,
                })?;
            return Ok(serde_json::json!({
                "from": from_str,
                "to": to_str,
                "copied": true,
                "success": true,
            }));
        }

        match tokio::fs::rename(&canonical_from, &final_dest).await {
            Ok(()) => {}
            // rename(2) cannot cross mounts (agent home -> granted /mnt folder).
            // Regular files fall back to copy + remove; directories still error.
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                crate::workspace::copy_then_remove(&canonical_from, &final_dest)
                    .await
                    .map_err(|reason| AgentOSError::ToolExecutionFailed {
                        tool_name: "file-move".into(),
                        reason,
                    })?;
            }
            Err(e) => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "file-move".into(),
                    reason: format!("Cannot move {} to {}: {}", from_str, to_str, e),
                });
            }
        }

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

#[cfg(all(test, unix))]
mod tests {
    #[tokio::test]
    async fn copy_fallback_refuses_dangling_symlink_destination() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src.txt");
        let outside = tmp.path().join("outside.txt");
        let link = tmp.path().join("link.txt");
        std::fs::write(&src, "payload").unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        assert!(crate::workspace::copy_then_remove(&src, &link)
            .await
            .is_err());
        assert!(!outside.exists(), "wrote through the symlink");
        assert!(src.exists(), "source must survive a refused move");

        let dest = tmp.path().join("dest.txt");
        crate::workspace::copy_then_remove(&src, &dest)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "payload");
        assert!(!src.exists());
    }
}
