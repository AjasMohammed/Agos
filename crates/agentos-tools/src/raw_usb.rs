use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp, PermissionSet};
use async_trait::async_trait;
use serde_json::Value;

pub struct RawUsbTool;

impl RawUsbTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RawUsbTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for RawUsbTool {
    fn name(&self) -> &str {
        "raw-usb"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("hardware.raw-usb.list".to_string(), PermissionOp::Read),
            (
                "hardware.raw-usb.session".to_string(),
                PermissionOp::Execute,
            ),
            ("hardware.raw-usb.transfer".to_string(), PermissionOp::Read),
            ("hardware.raw-usb.transfer".to_string(), PermissionOp::Write),
            ("hardware.raw-usb.control".to_string(), PermissionOp::Read),
            ("hardware.raw-usb.control".to_string(), PermissionOp::Write),
        ]
    }

    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        match payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => vec![("hardware.raw-usb.list".to_string(), PermissionOp::Read)],
            "open" | "close" => {
                vec![(
                    "hardware.raw-usb.session".to_string(),
                    PermissionOp::Execute,
                )]
            }
            "read" => vec![("hardware.raw-usb.transfer".to_string(), PermissionOp::Read)],
            "write" => vec![("hardware.raw-usb.transfer".to_string(), PermissionOp::Write)],
            "control" => {
                let op = match payload.get("direction").and_then(Value::as_str) {
                    Some("in") => PermissionOp::Read,
                    _ => PermissionOp::Write,
                };
                vec![("hardware.raw-usb.control".to_string(), op)]
            }
            _ => vec![("hardware.raw-usb.list".to_string(), PermissionOp::Read)],
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
            "raw-usb",
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
        let tool = RawUsbTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "list" })),
            vec![("hardware.raw-usb.list".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "write" })),
            vec![("hardware.raw-usb.transfer".to_string(), PermissionOp::Write)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "control", "direction": "in" })),
            vec![("hardware.raw-usb.control".to_string(), PermissionOp::Read)]
        );
    }
}
