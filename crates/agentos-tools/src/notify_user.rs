use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Fire-and-forget notification to the user.
///
/// The agent provides a subject and body; the kernel delivers it to the user
/// inbox and all registered delivery adapters (CLI, SSE, webhook, …).
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

        Ok(serde_json::json!({
            "_kernel_action": "notify_user",
            "subject": subject,
            "body": body,
            "priority": priority,
        }))
    }
}
