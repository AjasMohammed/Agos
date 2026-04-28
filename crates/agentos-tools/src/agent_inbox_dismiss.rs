use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct AgentInboxDismissTool;

impl AgentInboxDismissTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentInboxDismissTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AgentInboxDismissTool {
    fn name(&self) -> &str {
        "agent-inbox-dismiss"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let id = payload
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("agent-inbox-dismiss requires 'id'".into())
            })?
            .to_string();
        Ok(serde_json::json!({
            "_kernel_action": "agent_inbox_dismiss",
            "id": id,
        }))
    }
}
