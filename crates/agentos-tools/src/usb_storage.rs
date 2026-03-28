use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct UsbStorageTool;

impl UsbStorageTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for UsbStorageTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for UsbStorageTool {
    fn name(&self) -> &str {
        "usb-storage"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("hardware.usb-storage".to_string(), PermissionOp::Execute)]
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
        perms.grant("hardware.usb-storage".to_string(), false, false, true, None);

        hal.query(
            "usb-storage",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}
