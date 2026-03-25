use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct SysMonitorTool;

impl SysMonitorTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SysMonitorTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for SysMonitorTool {
    fn name(&self) -> &str {
        "sys-monitor"
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

        // SAFETY: Built-in tool — Kernel has already verified the agent's capability token
        // includes the required permissions before calling execute(). The self-grant here
        // matches what required_permissions() declares (hardware.system:r).
        // TODO: Thread real PermissionSet through ToolExecutionContext to avoid self-granting.
        let mut perms = agentos_types::PermissionSet::new();
        perms.grant("hardware.system".to_string(), true, false, false, None);
        debug_assert!(
            self.required_permissions()
                .iter()
                .all(|(res, _)| res == "hardware.system"),
            "Self-granted permissions must be a subset of required_permissions()"
        );

        hal.query(
            "system",
            serde_json::json!({}),
            &perms,
            Some(&context.agent_id),
        )
        .await
    }
}
