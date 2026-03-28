use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct LogReaderTool;

impl LogReaderTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LogReaderTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for LogReaderTool {
    fn name(&self) -> &str {
        "log-reader"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("fs.app_logs".to_string(), PermissionOp::Read),
            ("fs.system_logs".to_string(), PermissionOp::Read),
        ]
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
        perms.grant("fs.app_logs".to_string(), true, false, false, None);
        perms.grant("fs.system_logs".to_string(), true, false, false, None);

        hal.query(
            "log",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}
