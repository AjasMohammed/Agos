use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// List all event subscriptions belonging to the calling agent.
pub struct EventListSubscriptionsTool;

impl EventListSubscriptionsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventListSubscriptionsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventListSubscriptionsTool {
    fn name(&self) -> &str {
        "event-list-subscriptions"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("events.stream".to_string(), PermissionOp::Observe)]
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "_kernel_action": "event_list_subscriptions",
        }))
    }
}
