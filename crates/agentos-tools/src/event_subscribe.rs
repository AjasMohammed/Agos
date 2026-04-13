use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Subscribe the calling agent to events matching a filter.
///
/// Returns a `_kernel_action` payload that the kernel intercepts and
/// dispatches via [`KernelAction::EventSubscribeAction`]. The kernel
/// performs per-category permission gating before creating the subscription.
pub struct EventSubscribeTool;

impl EventSubscribeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventSubscribeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventSubscribeTool {
    fn name(&self) -> &str {
        "event-subscribe"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("events.stream".to_string(), PermissionOp::Observe)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let event_filter = payload
            .get("event_filter")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "event-subscribe requires 'event_filter' (e.g. 'category:HardwareEvents' or 'AgentAdded')".into(),
                )
            })?;

        let payload_filter = payload
            .get("payload_filter")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let throttle = payload
            .get("throttle")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let priority = payload
            .get("priority")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        Ok(serde_json::json!({
            "_kernel_action": "event_subscribe",
            "event_filter": event_filter,
            "payload_filter": payload_filter,
            "throttle": throttle,
            "priority": priority,
        }))
    }
}
