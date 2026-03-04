use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct TaskDelegate;

impl TaskDelegate {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl AgentTool for TaskDelegate {
    fn name(&self) -> &str { "task-delegate" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.message".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let target_agent = payload.get("agent").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "task-delegate requires 'agent' field".into()
            ))?;
        let task = payload.get("task").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "task-delegate requires 'task' field (the prompt for the sub-agent)".into()
            ))?;
        let priority = payload.get("priority").and_then(|v| v.as_u64()).unwrap_or(5) as u8;

        Ok(serde_json::json!({
            "_kernel_action": "delegate_task",
            "target_agent": target_agent,
            "task": task,
            "priority": priority,
        }))
    }
}
