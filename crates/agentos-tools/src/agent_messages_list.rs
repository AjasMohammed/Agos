use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct AgentMessagesListTool;

impl AgentMessagesListTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentMessagesListTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AgentMessagesListTool {
    fn name(&self) -> &str {
        "agent-messages-list"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let limit = payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(50) as u32;
        let unread_only = payload
            .get("unread_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        Ok(serde_json::json!({
            "_kernel_action": "agent_messages_list",
            "limit": limit,
            "unread_only": unread_only,
        }))
    }
}
