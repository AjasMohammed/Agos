use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Cancel one of the calling agent's own event subscriptions by ID.
pub struct EventUnsubscribeTool;

impl EventUnsubscribeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventUnsubscribeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventUnsubscribeTool {
    fn name(&self) -> &str {
        "event-unsubscribe"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("events.stream".to_string(), PermissionOp::Observe)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let subscription_id = payload
            .get("subscription_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "event-unsubscribe requires 'subscription_id' from event-list-subscriptions"
                        .into(),
                )
            })?;

        Ok(serde_json::json!({
            "_kernel_action": "event_unsubscribe",
            "subscription_id": subscription_id,
        }))
    }
}
