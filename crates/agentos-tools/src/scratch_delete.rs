use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_scratch::ScratchpadStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ScratchDeleteTool {
    store: Arc<ScratchpadStore>,
}

impl ScratchDeleteTool {
    pub fn new(store: Arc<ScratchpadStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for ScratchDeleteTool {
    fn name(&self) -> &str {
        "scratch-delete"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("scratchpad".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("scratchpad", PermissionOp::Write) {
            return Err(AgentOSError::PermissionDenied {
                resource: "scratchpad".to_string(),
                operation: format!("{:?}", PermissionOp::Write),
            });
        }

        let title = payload
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "scratch-delete requires 'title' field (string)".into(),
                )
            })?;

        let agent_id = context.agent_id.to_string();
        self.store
            .delete_page(&agent_id, title)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "scratch-delete".into(),
                reason: format!("Delete failed: {}", e),
            })?;

        Ok(serde_json::json!({
            "success": true,
            "deleted": title,
        }))
    }
}
