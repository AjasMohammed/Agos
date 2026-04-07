use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct PrinterTool;

impl PrinterTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PrinterTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for PrinterTool {
    fn name(&self) -> &str {
        "printer"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("hardware.printer".to_string(), PermissionOp::Execute)]
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
        perms.grant_op("hardware.printer".to_string(), PermissionOp::Execute, None);

        hal.query(
            "printer",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}
