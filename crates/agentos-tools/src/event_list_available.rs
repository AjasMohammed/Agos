use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Enumerate every event category and event type in the kernel, marking
/// which categories the calling agent has permission to subscribe to.
///
/// Use this tool first to discover what is available, then call
/// `event-subscribe` with a chosen filter.
pub struct EventListAvailableTool;

impl EventListAvailableTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventListAvailableTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventListAvailableTool {
    fn name(&self) -> &str {
        "event-list-available"
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
            "_kernel_action": "event_list_available",
        }))
    }
}
