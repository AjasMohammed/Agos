use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct SystemOpenFilesTool;

impl SystemOpenFilesTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemOpenFilesTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for SystemOpenFilesTool {
    fn name(&self) -> &str {
        "system-open-files"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("system.open_files".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: Value,
        context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        let hal = context
            .hal
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "Hardware Abstraction Layer (HAL) not available in this context"
                    .to_string(),
            })?;

        let mut perms = agentos_types::PermissionSet::new();
        perms.grant("system.open_files".to_string(), true, false, false, None);

        hal.query(
            "open_files",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}
