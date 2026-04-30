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
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("channel-send requires 'text' field".into())
            })?
            .to_string();

        if text.is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "channel-send 'text' must be non-empty".into(),
            ));
        }

        let thread_id = payload
            .get("thread_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(serde_json::json!({
            "_kernel_action": "channel_send",
            "channel": channel,
            "text": text,
            "thread_id": thread_id,
        }))
    }
}
