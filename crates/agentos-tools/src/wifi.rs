use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp, PermissionSet};
use async_trait::async_trait;
use serde_json::Value;

pub struct WifiTool;

impl WifiTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WifiTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for WifiTool {
    fn name(&self) -> &str {
        "wifi"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("hardware.wifi.list".to_string(), PermissionOp::Read),
            ("hardware.wifi.scan".to_string(), PermissionOp::Observe),
            (
                "hardware.wifi.connection".to_string(),
                PermissionOp::Execute,
            ),
            ("hardware.wifi.radio".to_string(), PermissionOp::Execute),
        ]
    }

    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        match payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
        {
            "status" => vec![("hardware.wifi.list".to_string(), PermissionOp::Read)],
            "scan" => vec![("hardware.wifi.scan".to_string(), PermissionOp::Observe)],
            "connect" | "disconnect" => vec![(
                "hardware.wifi.connection".to_string(),
                PermissionOp::Execute,
            )],
            "radio" => vec![("hardware.wifi.radio".to_string(), PermissionOp::Execute)],
            _ => vec![("hardware.wifi.list".to_string(), PermissionOp::Read)],
        }
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

        let mut perms = PermissionSet::new();
        for (resource, op) in self.required_permissions_for(&payload) {
            perms.grant_op(resource, op, None);
        }

        hal.query(
            "wifi",
            payload,
            &perms,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_permissions_are_scoped() {
        let tool = WifiTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "status" })),
            vec![("hardware.wifi.list".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "scan" })),
            vec![("hardware.wifi.scan".to_string(), PermissionOp::Observe)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "connect", "ssid": "x" })),
            vec![(
                "hardware.wifi.connection".to_string(),
                PermissionOp::Execute
            )]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "radio", "enabled": false })),
            vec![("hardware.wifi.radio".to_string(), PermissionOp::Execute)]
        );
    }

    /// A missing `action` must not silently inherit a stronger permission.
    #[test]
    fn default_and_unknown_actions_use_the_narrowest_permission() {
        let tool = WifiTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({})),
            vec![("hardware.wifi.list".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "exfiltrate" })),
            vec![("hardware.wifi.list".to_string(), PermissionOp::Read)]
        );
    }
}
