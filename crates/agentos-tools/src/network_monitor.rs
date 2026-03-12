use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct NetworkMonitorTool;

impl NetworkMonitorTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NetworkMonitorTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for NetworkMonitorTool {
    fn name(&self) -> &str {
        "network-monitor"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.logs".to_string(), PermissionOp::Read)]
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
        perms.grant("network.logs".to_string(), true, false, false, None);

        hal.query("network", payload, &perms).await
    }
}
