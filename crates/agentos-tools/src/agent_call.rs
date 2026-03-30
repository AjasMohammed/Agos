use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Synchronous agent-to-agent RPC tool.
///
/// Asks another agent to perform a task and blocks until it returns a result.
/// The kernel mediates the call: creates a child task for the target agent,
/// waits for it to complete, and returns the output to the caller.
pub struct AgentCallTool;

impl AgentCallTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentCallTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AgentCallTool {
    fn name(&self) -> &str {
        "agent-call"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.call".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let target_agent = payload
            .get("target_agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("agent-call requires 'target_agent' field".into())
            })?;

        let prompt = payload
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "agent-call requires 'prompt' field (the task for the target agent)".into(),
                )
            })?;

        let timeout_secs = payload
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(300)
            .min(3600); // Cap at 1 hour

        // Return a kernel action — the kernel will handle the RPC orchestration.
        // The dispatch_kernel_action handler blocks until the child task completes,
        // making this a synchronous call from the caller's perspective.
        Ok(serde_json::json!({
            "_kernel_action": "agent_rpc_call",
            "target_agent": target_agent,
            "prompt": prompt,
            "timeout_secs": timeout_secs,
        }))
    }
}
