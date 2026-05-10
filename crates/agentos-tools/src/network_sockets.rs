use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct NetworkSocketsTool;

impl NetworkSocketsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NetworkSocketsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for NetworkSocketsTool {
    fn name(&self) -> &str {
        "network-sockets"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.sockets".to_string(), PermissionOp::Read)]
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
        perms.grant("network.sockets".to_string(), true, false, false, None);

        hal.query(
            "network_sockets",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}
