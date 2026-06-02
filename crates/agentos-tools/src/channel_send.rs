use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Send a message to one specific connected channel.
///
/// Distinct from `notify-user`, which fans out to every registered delivery
/// adapter. `channel-send` is targeted: the agent picks one channel (by
/// display name or `ChannelInstanceID`).
///
/// The agent's currently-connected channels appear in its system prompt under
/// the `## Channels` block. Per-platform features (Telegram markdown, inline
/// keyboards, etc.) live in `agent-manual section=channel-<kind>`.
///
/// Requires `channel.send:w` permission.
pub struct ChannelSendTool;

impl ChannelSendTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ChannelSendTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ChannelSendTool {
    fn name(&self) -> &str {
        "channel-send"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("channel.send".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let channel = payload
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "channel-send requires 'channel' (display name or ID)".into(),
                )
            })?
            .to_string();

        let text = payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // Optional media attachment by URL. The channel fetches the URL itself
        // (Telegram sendPhoto/sendDocument). `image_url` and `document_url` are
        // mutually exclusive; `caption` annotates the media.
        let image_url = payload
            .get("image_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let document_url = payload
            .get("document_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // A stored-file attachment by id (kernel resolves bytes and uploads
        // directly — no public URL needed). Telegram-only today.
        let file_id = payload
            .get("file_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // An album of image URLs (Telegram sendMediaGroup): 2–10 items.
        let image_urls: Vec<String> = payload
            .get("image_urls")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if image_urls.len() > 10 {
            return Err(AgentOSError::SchemaValidation(
                "channel-send: 'image_urls' album supports at most 10 items".into(),
            ));
        }

        let url_sources = [
            image_url.is_some(),
            document_url.is_some(),
            file_id.is_some(),
            !image_urls.is_empty(),
        ];
        if url_sources.iter().filter(|s| **s).count() > 1 {
            return Err(AgentOSError::SchemaValidation(
                "channel-send: provide only one of 'image_url', 'document_url', 'file_id', or 'image_urls'"
                    .into(),
            ));
        }

        // A message must carry something: text or a media source.
        if text.is_empty()
            && image_url.is_none()
            && document_url.is_none()
            && file_id.is_none()
            && image_urls.is_empty()
        {
            return Err(AgentOSError::SchemaValidation(
                "channel-send requires non-empty 'text', or an 'image_url'/'document_url'/'file_id'/'image_urls'"
                    .into(),
            ));
        }

        let caption = payload
            .get("caption")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let filename = payload
            .get("filename")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let thread_id = payload
            .get("thread_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(serde_json::json!({
            "_kernel_action": "channel_send",
            "channel": channel,
            "text": text,
            "thread_id": thread_id,
            "image_url": image_url,
            "document_url": document_url,
            "file_id": file_id,
            "image_urls": image_urls,
            "caption": caption,
            "filename": filename,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::ToolExecutionContext;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
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
        }
    }

    async fn run(payload: serde_json::Value) -> Result<serde_json::Value, AgentOSError> {
        ChannelSendTool::new().execute(payload, ctx()).await
    }

    #[tokio::test]
    async fn text_only_still_works() {
        let out = run(serde_json::json!({ "channel": "tg", "text": "hi" }))
            .await
            .unwrap();
        assert_eq!(out["text"], "hi");
        assert!(out["image_url"].is_null());
    }

    #[tokio::test]
    async fn image_url_with_empty_text_is_allowed() {
        let out = run(serde_json::json!({
            "channel": "tg",
            "image_url": "https://example.com/cat.png",
            "caption": "a cat"
        }))
        .await
        .unwrap();
        assert_eq!(out["image_url"], "https://example.com/cat.png");
        assert_eq!(out["caption"], "a cat");
    }

    #[tokio::test]
    async fn empty_everything_is_rejected() {
        assert!(run(serde_json::json!({ "channel": "tg" })).await.is_err());
        assert!(run(serde_json::json!({ "channel": "tg", "text": "" }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn image_and_document_together_rejected() {
        assert!(run(serde_json::json!({
            "channel": "tg",
            "image_url": "https://example.com/a.png",
            "document_url": "https://example.com/b.pdf"
        }))
        .await
        .is_err());
    }
}
