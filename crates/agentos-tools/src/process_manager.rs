use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct ProcessManagerTool;

impl ProcessManagerTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProcessManagerTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ProcessManagerTool {
    fn name(&self) -> &str {
        "process-manager"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("process.list".to_string(), PermissionOp::Read),
            ("process.kill".to_string(), PermissionOp::Execute),
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

        let action = payload
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("list");

        let mut perms = agentos_types::PermissionSet::new();
        match action {
            "list" => {
                perms.grant("process.list".to_string(), true, false, false, None);
            }
            "kill" => {
                perms.grant("process.kill".to_string(), false, false, true, None);
            }
            _ => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Unsupported process-manager action: '{}'. Valid actions: 'list', 'kill'",
                    action
                )));
            }
        }

        hal.query("process", payload, &perms).await
    }
}
