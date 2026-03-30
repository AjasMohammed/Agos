use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

#[derive(Default)]
pub struct ContextMemoryUpdateTool;

impl ContextMemoryUpdateTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentTool for ContextMemoryUpdateTool {
    fn name(&self) -> &str {
        "context-memory-update"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.context".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("memory.context", PermissionOp::Write)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.context".to_string(),
                operation: format!("{:?}", PermissionOp::Write),
            });
        }

        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "context-memory-update requires 'content' field (string)".into(),
                )
            })?;

        let reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Return a kernel action for the dispatch loop to handle.
        // Write-own-only: agent_id is taken from the execution context, not the payload.
        Ok(serde_json::json!({
            "_kernel_action": "context_memory_update",
            "agent_id": context.agent_id.to_string(),
            "content": content,
            "reason": reason,
        }))
    }
}
