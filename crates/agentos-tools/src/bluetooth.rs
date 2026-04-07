use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp, PermissionSet};
use async_trait::async_trait;
use serde_json::Value;

pub struct BluetoothTool;

impl BluetoothTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BluetoothTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for BluetoothTool {
    fn name(&self) -> &str {
        "bluetooth"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("hardware.bluetooth.list".to_string(), PermissionOp::Read),
            ("hardware.bluetooth.scan".to_string(), PermissionOp::Observe),
            ("hardware.bluetooth.pair".to_string(), PermissionOp::Execute),
            (
                "hardware.bluetooth.connection".to_string(),
                PermissionOp::Execute,
            ),
            ("hardware.bluetooth.gatt".to_string(), PermissionOp::Read),
            ("hardware.bluetooth.gatt".to_string(), PermissionOp::Write),
        ]
    }

    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        match payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list_adapters")
        {
            "list_adapters" => vec![("hardware.bluetooth.list".to_string(), PermissionOp::Read)],
            "scan" => vec![("hardware.bluetooth.scan".to_string(), PermissionOp::Observe)],
            "pair" => vec![("hardware.bluetooth.pair".to_string(), PermissionOp::Execute)],
            "connect" | "disconnect" => vec![(
                "hardware.bluetooth.connection".to_string(),
                PermissionOp::Execute,
            )],
            "gatt_read" => vec![("hardware.bluetooth.gatt".to_string(), PermissionOp::Read)],
            "gatt_write" => vec![("hardware.bluetooth.gatt".to_string(), PermissionOp::Write)],
            _ => vec![("hardware.bluetooth.list".to_string(), PermissionOp::Read)],
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
            "bluetooth",
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
        let tool = BluetoothTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "list_adapters" })),
            vec![("hardware.bluetooth.list".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "scan" })),
            vec![("hardware.bluetooth.scan".to_string(), PermissionOp::Observe)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "gatt_write" })),
            vec![("hardware.bluetooth.gatt".to_string(), PermissionOp::Write)]
        );
    }
}
