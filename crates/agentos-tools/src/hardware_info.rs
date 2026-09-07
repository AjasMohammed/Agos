use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct HardwareInfoTool;

impl HardwareInfoTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HardwareInfoTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for HardwareInfoTool {
    fn name(&self) -> &str {
        "hardware-info"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("hardware.system".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        _payload: Value,
        context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        let hal = context
            .hal
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "Hardware Abstraction Layer (HAL) not available in this context"
                    .to_string(),
            })?;

        // Forward the agent's real grant (default agent permissions include
        // hardware.system:r) — the HAL-internal check then re-verifies the
        // agent's actual authority instead of a self-minted set.
        hal.query(
            "system",
            serde_json::json!({}),
            &context.permissions,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}
