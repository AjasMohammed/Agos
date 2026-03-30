use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

#[derive(Default)]
pub struct ContextMemoryReadTool;

impl ContextMemoryReadTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentTool for ContextMemoryReadTool {
    fn name(&self) -> &str {
        "context-memory-read"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.context".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("memory.context", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.context".to_string(),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        // Return a kernel action for the dispatch loop to handle.
        // Write-own-only: agent_id is taken from the execution context, not the payload.
        Ok(serde_json::json!({
            "_kernel_action": "context_memory_read",
            "agent_id": context.agent_id.to_string(),
        }))
    }
}
