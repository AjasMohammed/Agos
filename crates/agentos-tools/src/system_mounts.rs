use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct SystemMountsTool;

impl SystemMountsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemMountsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for SystemMountsTool {
    fn name(&self) -> &str {
        "system-mounts"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("system.mounts".to_string(), PermissionOp::Read)]
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
        perms.grant("system.mounts".to_string(), true, false, false, None);

        hal.query(
            "mounts",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}
