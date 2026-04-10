use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Delegate a task to an external A2A-compliant agent.
///
/// Uses the `_kernel_action: "a2a_delegate"` pattern — the tool returns
/// a structured action and the kernel performs the actual HTTP call,
/// applying CapabilityToken auth and writing audit entries.
pub struct A2ADelegateTool;

impl A2ADelegateTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for A2ADelegateTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for A2ADelegateTool {
    fn name(&self) -> &str {
        "a2a-delegate"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("network.outbound".to_string(), PermissionOp::Execute),
            ("a2a.delegate".to_string(), PermissionOp::Execute),
        ]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let agent_url = payload
            .get("agent_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("a2a-delegate requires 'agent_url' field".into())
            })?;

        let capability = payload
            .get("capability")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("a2a-delegate requires 'capability' field".into())
            })?;

        let input = payload
            .get("input")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        let token = payload
            .get("token")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let wait_for_result = payload
            .get("wait_for_result")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        tracing::debug!(
            agent_url = %agent_url,
            capability = %capability,
            wait_for_result = wait_for_result,
            "a2a_delegate tool: forwarding to kernel"
        );

        Ok(serde_json::json!({
            "_kernel_action": "a2a_delegate",
            "agent_url": agent_url,
            "capability": capability,
            "input": input,
            "token": token,
            "wait_for_result": wait_for_result,
        }))
    }
}
