use crate::file_lock::WriteLockGuard;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;

pub struct FileEditor;

impl FileEditor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileEditor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for FileEditor {
    fn name(&self) -> &str {
        "file-editor"
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
                AgentOSError::SchemaValidation("file-editor requires 'path' field".into())
            })?;

        let edits = payload
            .get("edits")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("file-editor requires 'edits' array field".into())
            })?;

        if edits.is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "file-editor: 'edits' array must not be empty".into(),
            ));
        }

        tracing::debug!(
            path = path_str,
            edits_count = edits.len(),
            "file-editor: starting"
        );

        // Parse edits upfront so we fail fast on malformed input.
        let mut parsed_edits: Vec<(String, String, bool)> = Vec::with_capacity(edits.len());
        for (i, edit) in edits.iter().enumerate() {
            let old_text = edit
                .get("old_text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    AgentOSError::SchemaValidation(format!(
                        "file-editor: edit[{}] missing 'old_text'",
                        i
                    ))
                })?
                .to_string();
            // "".matches("") is non-zero: an empty needle would splice new_text
            // between every character.
            if old_text.is_empty() {
                return Err(AgentOSError::SchemaValidation(format!(
                    "file-editor: edit[{}] 'old_text' must not be empty",
                    i
                )));
            }
            let new_text = edit
                .get("new_text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    AgentOSError::SchemaValidation(format!(
                        "file-editor: edit[{}] missing 'new_text'",
                        i
                    ))
                })?
                .to_string();
            let replace_all = edit
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            parsed_edits.push((old_text, new_text, replace_all));
        }

        // SECURITY: relative paths resolve under the agent's own home, never the
        // kernel state dir (audit.db, api_keys.db, chat.db, agents.json live there).
        let agent_root = context.agent_files_dir()?;
        // SECURITY: file-editor writes — resolve against the *writable* workspace list.
        let resolved = crate::traits::resolve_tool_path(
            path_str,
            &agent_root,
            // Writable grants AND read-write storage zones — the conversation's
            // shared workspace is a zone, and the resolver rejects an unknown
            // absolute path before the zone check further down ever runs.
            &context.write_roots(),
        )
        .map_err(|e| context.with_path_hint(e))?;

        let canonical = resolved
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-editor".into(),
                reason: format!("Path not found: {} ({})", path_str, e),
            })?;

        let canonical_agent_root =
            agent_root
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-editor".into(),
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
            tracing::warn!(path = path_str, "file-editor: path traversal blocked");
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

        // HIGH-1: Size guard — prevent OOM on huge files, consistent with file_reader.
        const MAX_EDIT_FILE_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB
        let size_bytes = tokio::fs::metadata(&canonical)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if size_bytes > MAX_EDIT_FILE_BYTES {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "file-editor".into(),
                reason: format!(
                    "File too large for editing: {} bytes (limit {} bytes).",
                    size_bytes, MAX_EDIT_FILE_BYTES
                ),
            });
        }

        // Acquire write lock before reading — holds across the full read-modify-write cycle.
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

        // Read current content.
        let mut content = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "file-editor".into(),
                reason: format!("Cannot read {}: {}", path_str, e),
            }
        })?;

        // Apply each edit sequentially.
        let mut replacements = 0usize;
        for (i, (old_text, new_text, replace_all)) in parsed_edits.iter().enumerate() {
            let occurrences = content.matches(old_text.as_str()).count();
            if occurrences == 0 {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "file-editor".into(),
                    reason: format!(
                        "edit[{}]: 'old_text' not found in file: {:?}",
                        i,
                        truncate_for_display(old_text, 80)
                    ),
                });
            }
            if occurrences > 1 && !replace_all {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "file-editor".into(),
                    reason: format!(
                        "edit[{}]: 'old_text' appears {} times — must be unique. Provide more surrounding context, or set replace_all=true on this edit.",
                        i, occurrences
                    ),
                });
            }
            content = content.replace(old_text.as_str(), new_text.as_str());
            replacements += occurrences;
        }

        // Atomic write via tmp + rename.
        let bytes_written = content.len() as u64;
        let backup = crate::workspace::backup(&canonical_agent_root, &canonical).await;
        crate::workspace::atomic_write("file-editor", &canonical, &content).await?;

        tracing::debug!(
            path = path_str,
            edits_applied = parsed_edits.len(),
            bytes_written,
            "file-editor: complete"
        );

        Ok(serde_json::json!({
            "path": path_str,
            "edits_applied": parsed_edits.len(),
            "replacements": replacements,
            "backup": backup,
            "bytes_written": bytes_written,
            "success": true,
        }))
    }
}

fn truncate_for_display(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Walk back to the nearest char boundary to avoid panicking on multi-byte UTF-8.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_at_char_boundary() {
        // ASCII only — no adjustment needed.
        assert_eq!(truncate_for_display("hello world", 5), "hello");
        assert_eq!(truncate_for_display("hi", 100), "hi");
        assert_eq!(truncate_for_display("", 5), "");
    }

    #[test]
    fn truncate_mid_multibyte_walks_back() {
        // 'é' (U+00E9) encodes as 2 bytes: 0xC3 0xA9.
        // "café" is 5 bytes: c(1) a(1) f(1) é(2).
        let s = "caf\u{00e9}!"; // 6 bytes total

        // Cutting at byte 4 lands in the middle of 'é' — must walk back to 3.
        assert_eq!(truncate_for_display(s, 4), "caf");
        // Cutting at byte 5 is a valid boundary (after 'é', before '!').
        assert_eq!(truncate_for_display(s, 5), "caf\u{00e9}");
        // No truncation needed.
        assert_eq!(truncate_for_display(s, 6), s);
    }

    #[test]
    fn truncate_all_multibyte_walks_back_to_zero() {
        // A 3-byte character (€ = U+20AC, encoded as 0xE2 0x82 0xAC).
        let s = "\u{20AC}"; // 3 bytes, no ASCII prefix
                            // max=1 and max=2 are both mid-char — must walk back to 0.
        assert_eq!(truncate_for_display(s, 1), "");
        assert_eq!(truncate_for_display(s, 2), "");
        assert_eq!(truncate_for_display(s, 3), s);
    }
}
