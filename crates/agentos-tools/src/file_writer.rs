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

        // SECURITY: resolve path, checking workspace paths before falling back to data_dir.
        let resolved =
            crate::traits::resolve_tool_path(path_str, &context.data_dir, &context.workspace_paths);

        // Normalize lexically (can't use canonicalize — file may not exist yet).
        let normalized = normalize_path(&resolved);
        let canonical_data_dir =
            context
                .data_dir
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        let in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| normalized.starts_with(wp));
        if !normalized.starts_with(&canonical_data_dir) && !in_workspace {
            tracing::warn!(path = path_str, "file-writer: path traversal blocked");
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
                .workspace_paths
                .iter()
                .any(|wp| canonical_parent.starts_with(wp));
            if !canonical_parent.starts_with(&canonical_data_dir) && !parent_in_workspace {
                return Err(AgentOSError::PermissionDenied {
                    resource: "fs.user_data".into(),
                    operation: format!("Path traversal denied: {}", path_str),
                });
            }
            canonical_parent.join(
                normalized
                    .file_name()
                    .ok_or_else(|| AgentOSError::SchemaValidation("Path has no filename".into()))?,
            )
        } else {
            normalized.clone()
        };

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
                atomic_write(&final_path, content).await?;
            }
            "append" => {
                use tokio::io::AsyncWriteExt;
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
                atomic_write(&final_path, content).await?;
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
            "success": true,
        }))
    }
}

/// Write content to a `.tmp` sibling file then atomically rename it to `target`.
/// This ensures readers never observe a partial write.
async fn atomic_write(target: &PathBuf, content: &str) -> Result<(), AgentOSError> {
    let tmp = target.with_extension("tmp");
    tokio::fs::write(&tmp, content)
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "file-writer".into(),
            reason: format!("Temp write failed: {}", e),
        })?;
    tokio::fs::rename(&tmp, target)
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "file-writer".into(),
            reason: format!("Atomic rename failed: {}", e),
        })
}

fn normalize_path(path: &Path) -> PathBuf {
    crate::workspace::normalize_path(path)
}
