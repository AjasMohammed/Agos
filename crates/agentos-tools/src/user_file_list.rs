use crate::traits::{AgentTool, ToolExecutionContext};
use crate::user_files::UserFiles;
use agentos_types::*;
use async_trait::async_trait;

/// Default page size. Enough that "what did the user send me?" is answered in
/// one call for any realistic session.
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

/// Lists what the user has uploaded.
///
/// Until this existed, `user-file-reader` was the only route into the upload
/// registry and it needed a `file_id` or an exact `file_name` up front. An agent
/// that had neither could only interrogate the user for a UUID — which is what
/// happened in the session this tool was written for, ending with the agent
/// telling the user to re-upload a file that was on disk the whole time.
///
/// It also dissolves two problems that look unrelated:
///
/// - **Cross-session ids.** A `file_id` is minted in one session's attachment
///   note; another session has no route back to it. Listing is that route.
/// - **The inbound name dead-end.** Channel media is deliberately not
///   addressable by name (a sender would otherwise be able to make any filename
///   resolve to their own file). Listing returns those rows with distinct ids
///   and a `source` field, so the agent chooses in the open rather than being
///   silently handed the wrong file. Disclosure is the safe direction here;
///   silent resolution is the one that was dangerous.
pub struct UserFileListTool;

impl UserFileListTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for UserFileListTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for UserFileListTool {
    fn name(&self) -> &str {
        "user-file-list"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Same permission as `user-file-reader`: this is strictly less than what
        // that tool already grants (metadata, no content), so an agent that can
        // read uploads can list them without a new grant.
        vec![("fs.user_data".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mime_prefix = payload
            .get("mime_prefix")
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_lowercase());
        let limit = payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(1, MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        // No registry yet is not an error: "nothing has been uploaded" is a
        // complete, actionable answer, and returning it as a failure pushed the
        // model into retrying or blaming its own arguments.
        let Some(files) = UserFiles::open(&context.data_dir) else {
            return Ok(serde_json::json!({
                "count": 0,
                "files": [],
                "note": "No files have been uploaded yet.",
            }));
        };

        // One extra row, discarded before rendering: it is the difference between
        // "there are more" and "there happened to be exactly this many", and the
        // note tells the agent to narrow rather than page.
        let records = tokio::task::spawn_blocking(move || {
            files.list(query.as_deref(), mime_prefix.as_deref(), limit + 1)
        })
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "user-file-list".into(),
            reason: format!("spawn_blocking panicked: {e}"),
        })?
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "user-file-list".into(),
            reason: e,
        })?;

        // `path` is deliberately absent from every row: it points inside the
        // kernel state dir, beside `audit.db` and `api_keys.db`. An agent that
        // needs a path asks `user-file-reader` for a handle, which materializes a
        // copy under the agent's own root.
        //
        // `name` and `original_name` are sender-chosen for inbound rows. They
        // stay JSON string values and are never folded into prose, so the
        // injection scan `Ok` tool results pass through in
        // `task_executor::push_tool_result` sees them as the untrusted data
        // they are.
        let truncated = records.len() > limit;
        let entries: Vec<serde_json::Value> = records
            .iter()
            .take(limit)
            .map(|r| {
                serde_json::json!({
                    "file_id":     r.id,
                    "name":        r.name,
                    "mime":        r.mime,
                    "size_bytes":  r.size,
                    "uploaded_at": r.uploaded_at,
                    "source":      r.source(),
                    "readable_as": r.readable_as(),
                })
            })
            .collect();

        let mut out = serde_json::json!({
            "count": entries.len(),
            "files": entries,
        });
        if truncated {
            out["note"] = serde_json::Value::String(format!(
                "Showing the {limit} most recent. Narrow with 'query' or 'mime_prefix' rather \
                 than raising 'limit'."
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_files::tests::register;
    use std::path::Path;
    use tempfile::TempDir;

    fn ctx(data_dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
            shared_dir: None,
        }
    }

    async fn list(dir: &TempDir, payload: serde_json::Value) -> serde_json::Value {
        UserFileListTool::new()
            .execute(payload, ctx(dir.path()))
            .await
            .unwrap()
    }

    /// The failing session in one test: an agent that knows only "the user sent
    /// an audio file" finds it, with the id it needs to act.
    #[tokio::test]
    async fn finds_channel_audio_the_name_lookup_cannot_reach() {
        let dir = TempDir::new().unwrap();
        let id = register(dir.path(), "song.mp3", "audio/mpeg", b"x", "inbound");
        register(dir.path(), "notes.txt", "text/plain", b"y", "");

        let out = list(&dir, serde_json::json!({ "mime_prefix": "audio/" })).await;
        assert_eq!(out["count"], 1);
        let f = &out["files"][0];
        assert_eq!(f["file_id"], id);
        assert_eq!(f["source"], "channel");
        assert_eq!(f["readable_as"], "binary");
    }

    /// A path here would point an agent at the kernel state dir.
    #[tokio::test]
    async fn never_returns_a_path_or_content() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "notes.txt", "text/plain", b"secret", "");
        let out = list(&dir, serde_json::json!({})).await;
        let body = out.to_string();
        assert!(!body.contains("uploads"), "leaked a path: {body}");
        assert!(!body.contains("secret"), "leaked content: {body}");
        assert!(out["files"][0].get("path").is_none());
    }

    #[tokio::test]
    async fn an_empty_registry_is_an_answer_not_an_error() {
        let dir = TempDir::new().unwrap();
        let out = list(&dir, serde_json::json!({})).await;
        assert_eq!(out["count"], 0);
        assert!(out["note"].as_str().unwrap().contains("No files"));
    }

    #[tokio::test]
    async fn limit_is_clamped_and_truncation_is_reported() {
        let dir = TempDir::new().unwrap();
        for i in 0..5 {
            register(dir.path(), &format!("f{i}.txt"), "text/plain", b"x", "");
        }
        let out = list(&dir, serde_json::json!({ "limit": 2 })).await;
        assert_eq!(out["count"], 2);
        assert!(out["note"].as_str().unwrap().contains("most recent"));

        // Over the cap clamps rather than erroring; under-cap results carry no note.
        let out = list(&dir, serde_json::json!({ "limit": 9_000 })).await;
        assert_eq!(out["count"], 5);
        assert!(out.get("note").is_none(), "got {out}");
    }

    /// `artifact-write` registers into the same table with the same scope. An
    /// agent that has written a few reports must still see the user's file, not
    /// twenty of its own.
    #[tokio::test]
    async fn agent_written_artifacts_are_not_listed_as_uploads() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "song.mp3", "audio/mpeg", b"x", "inbound");
        for i in 0..25 {
            register(
                dir.path(),
                &format!("report{i}.md"),
                "text/markdown",
                b"y",
                "artifact,kind:markdown",
            );
        }
        let out = list(&dir, serde_json::json!({})).await;
        assert_eq!(out["count"], 1, "got {out}");
        assert_eq!(out["files"][0]["name"], "song.mp3");
    }

    #[tokio::test]
    async fn query_filters_by_name() {
        let dir = TempDir::new().unwrap();
        register(dir.path(), "report.pdf", "application/pdf", b"x", "");
        register(dir.path(), "song.mp3", "audio/mpeg", b"y", "");
        let out = list(&dir, serde_json::json!({ "query": "report" })).await;
        assert_eq!(out["count"], 1);
        assert_eq!(out["files"][0]["name"], "report.pdf");
    }
}
