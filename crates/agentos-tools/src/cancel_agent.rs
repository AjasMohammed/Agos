use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Cancel a spawned sub-agent that is going off-track or no longer needed.
///
/// Uses the `_kernel_action: "cancel_agent"` pattern so the kernel handles
/// the privileged task cancellation, including cascading to grandchildren.
pub struct CancelAgentTool;

impl CancelAgentTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CancelAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for CancelAgentTool {
    fn name(&self) -> &str {
        "cancel-agent"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.spawn".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let task_id = payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("cancel-agent requires 'task_id' string".into())
            })?;

        let reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("Cancelled by parent agent");

        tracing::debug!(
            task_id = %task_id,
            reason = %reason,
            "cancel_agent tool: forwarding to kernel"
        );

        Ok(serde_json::json!({
            "_kernel_action": "cancel_agent",
            "task_id": task_id,
            "reason": reason,
        }))
    }
}
