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

        let mut perms = agentos_types::PermissionSet::new();
        perms.grant("hardware.system".to_string(), true, false, false, None);

        hal.query("system", serde_json::json!({}), &perms).await
    }
}
