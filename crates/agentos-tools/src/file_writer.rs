use crate::file_lock::WriteLockGuard;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB

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

        // Resolve write mode. `mode` takes precedence; `append: true` is legacy compat.
        let mode = if let Some(m) = payload.get("mode").and_then(|v| v.as_str()) {
            match m {
                "overwrite" | "append" | "create_only" => m.to_string(),
                other => {
                    return Err(AgentOSError::SchemaValidation(format!(
                        "file-writer: unknown mode '{}'; expected overwrite | append | create_only",
                        other
                    )))
                }
            }
        } else if payload
            .get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            "append".to_string()
        } else {
            "overwrite".to_string()
        };

        // Size guard.
        let max_bytes = payload
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_MAX_BYTES);
        let content_bytes = content.len() as u64;
        if content_bytes > max_bytes {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "file-writer".into(),
                reason: format!(
                    "Content size {} bytes exceeds limit of {} bytes",
                    content_bytes, max_bytes
                ),
            });
        }

        tracing::debug!(
            path = path_str,
            mode = mode.as_str(),
            bytes = content_bytes,
            "file-writer: starting"
        );

        // SECURITY: relative paths resolve under the agent's own home, never the
        // kernel state dir (audit.db, api_keys.db, chat.db, agents.json live there).
        let agent_root = context.agent_files_dir()?;
        // SECURITY: resolve path against the *writable* workspace list. A
        // grant with mode `r` only does not appear here, so a read-only grant
        // cannot be written through even if the agent's `PermissionSet`
        // allows fs.user_data writes.
        let resolved = crate::traits::resolve_tool_path(
            path_str,
            &agent_root,
            // Writable grants AND read-write storage zones — the conversation's
            // shared workspace is a zone, and the resolver rejects an unknown
            // absolute path before the zone check further down ever runs.
            &context.write_roots(),
        )
        .map_err(|e| context.with_path_hint(e))?;

        // Normalize lexically (can't use canonicalize — file may not exist yet).
        let normalized = normalize_path(&resolved);
        let canonical_agent_root =
            agent_root
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        let in_workspace = context
            .workspace_paths_writable
            .iter()
            .any(|wp| normalized.starts_with(wp));
        // KMC Phase 3: check dynamic storage zones
        let in_storage_zone = context
            .storage_zone_query
            .as_ref()
            .map(|q| q.is_path_in_zone(&context.agent_id, &normalized))
            .unwrap_or(false);
        if !normalized.starts_with(&canonical_agent_root) && !in_workspace && !in_storage_zone {
            tracing::warn!(path = path_str, "file-writer: path traversal blocked");
            return Err(context.deny_path(path_str));
        }
        // KMC: enforce read-only zones — deny writes to ReadOnly storage zones.
        if in_storage_zone {
            let access = context
                .storage_zone_query
                .as_ref()
                .and_then(|q| q.zone_access(&context.agent_id, &normalized));
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

        // Acquire exclusive write lock. Held until the guard drops at end of scope.
        // If another agent holds the lock (reader or writer) this returns FileLocked.
        let _lock_guard = if let Some(registry) = &context.file_lock_registry {
            Some(WriteLockGuard::acquire(
                registry,
                normalized.clone(),
                context.agent_id,
                context.task_id,
            )?)
        } else {
            None
        };

        // Create parent directories (path is validated above).
        if let Some(parent) = normalized.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Cannot create directory: {}", e),
                }
            })?;
        }

        // CRITICAL: After creating parent dirs, canonicalize the destination's parent
        // and re-join the filename to detect symlinks in newly-created paths that could
        // escape data_dir or a workspace root — lexical normalize_path cannot catch these.
        let final_path = if let Some(parent) = normalized.parent() {
            let canonical_parent =
                parent
                    .canonicalize()
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "file-writer".into(),
                        reason: format!("Cannot resolve parent directory: {}", e),
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
                return Err(context.deny_path(path_str));
            }
            canonical_parent.join(
                normalized
                    .file_name()
                    .ok_or_else(|| AgentOSError::SchemaValidation("Path has no filename".into()))?,
            )
        } else {
            normalized.clone()
        };

        let mut backup = None;
        match mode.as_str() {
            "create_only" => {
                // Fail if the file already exists.
                if tokio::fs::metadata(&final_path).await.is_ok() {
                    return Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "file-writer".into(),
                        reason: format!("File already exists: {}", path_str),
                    });
                }
                // Atomic write: write to .tmp then rename.
                crate::workspace::atomic_write("file-writer", &final_path, content).await?;
            }
            "append" => {
                use tokio::io::AsyncWriteExt;
                // SECURITY: append opens the path in place, so a symlink leaf
                // (only the parent is canonicalized above) would be written
                // through to wherever it points. Overwrite is immune: rename
                // replaces the link itself.
                if tokio::fs::symlink_metadata(&final_path)
                    .await
                    .is_ok_and(|m| m.file_type().is_symlink())
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "fs.user_data".into(),
                        operation: format!("Refusing to append through a symlink: {}", path_str),
                    });
                }
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&final_path)
                    .await
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "file-writer".into(),
                        reason: format!("Cannot open for append: {}", e),
                    })?;
                file.write_all(content.as_bytes()).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "file-writer".into(),
                        reason: format!("Append failed: {}", e),
                    }
                })?;
            }
            _ => {
                // "overwrite" — atomic write via tmp + rename.
                backup = crate::workspace::backup(&canonical_agent_root, &final_path).await;
                crate::workspace::atomic_write("file-writer", &final_path, content).await?;
            }
        }

        tracing::debug!(
            path = path_str,
            bytes_written = content_bytes,
            mode = mode.as_str(),
            "file-writer: complete"
        );

        Ok(serde_json::json!({
            "path": path_str,
            "bytes_written": content_bytes,
            "mode": mode,
            "backup": backup,
            "success": true,
        }))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    crate::workspace::normalize_path(path)
}
