use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct AgentMessageTool;

impl AgentMessageTool {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl AgentTool for AgentMessageTool {
    fn name(&self) -> &str { "agent-message" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.message".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let to = payload.get("to").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "agent-message requires 'to' field (agent name)".into()
            ))?;
        let content = payload.get("content").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "agent-message requires 'content' field".into()
            ))?;

        Ok(serde_json::json!({
            "_kernel_action": "send_agent_message",
            "to": to,
            "content": content,
        }))
    }
}
