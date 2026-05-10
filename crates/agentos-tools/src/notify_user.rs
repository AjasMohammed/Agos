use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Fire-and-forget notification to the user.
///
/// The agent provides a subject and body; the kernel delivers it to the user
/// inbox and all registered delivery adapters (CLI, SSE, webhook, …) by default.
///
/// Optional `channels` field restricts delivery to one or more selected channels.
/// Each entry may be a channel kind (`telegram`, `whatsapp`, `slack`, `webhook`,
/// `desktop`, `cli`, `web`, `ntfy`, `email`, `discord`), a registered channel's
/// display name, or its `ChannelInstanceID`. Empty/omitted = fan out to all.
///
/// Requires `user.notify:w` permission.
pub struct NotifyUserTool;

impl NotifyUserTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NotifyUserTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for NotifyUserTool {
    fn name(&self) -> &str {
        "notify-user"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("user.notify".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let subject = payload
            .get("subject")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("notify-user requires 'subject' field".into())
            })?
            .to_string();

        let body = payload
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("notify-user requires 'body' field".into())
            })?
            .to_string();

        let priority = payload
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("info")
            .to_string();

        let channels: Vec<String> = match payload.get("channels") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(arr)) => {
                if arr.is_empty() {
                    return Err(AgentOSError::SchemaValidation(
                        "notify-user 'channels' is empty — omit the field to fan out to all channels, or include at least one selector".into(),
                    ));
                }
                let mut out = Vec::with_capacity(arr.len());
                for v in arr {
                    let s = v.as_str().ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "notify-user 'channels' entries must be strings".into(),
                        )
                    })?;
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        return Err(AgentOSError::SchemaValidation(
                            "notify-user 'channels' contains an empty/whitespace entry".into(),
                        ));
                    }
                    out.push(trimmed.to_string());
                }
                out
            }
            Some(serde_json::Value::String(s)) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Err(AgentOSError::SchemaValidation(
                        "notify-user 'channels' is empty — omit the field to fan out to all channels".into(),
                    ));
                }
                vec![trimmed.to_string()]
            }
            _ => {
                return Err(AgentOSError::SchemaValidation(
                    "notify-user 'channels' must be a string or array of strings".into(),
                ));
            }
        };

        Ok(serde_json::json!({
            "_kernel_action": "notify_user",
            "subject": subject,
            "body": body,
            "priority": priority,
            "channels": channels,
        }))
    }
}
