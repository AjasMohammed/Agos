use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp, PermissionSet};
use async_trait::async_trait;
use serde_json::Value;

pub struct DisplayConfigTool;

impl DisplayConfigTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DisplayConfigTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for DisplayConfigTool {
    fn name(&self) -> &str {
        "display-config"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("hardware.display".to_string(), PermissionOp::Read),
            ("hardware.display".to_string(), PermissionOp::Query),
            ("hardware.display.config".to_string(), PermissionOp::Write),
        ]
    }

    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        match payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => vec![("hardware.display".to_string(), PermissionOp::Read)],
            "test" => vec![("hardware.display".to_string(), PermissionOp::Query)],
            "confirm" | "revert" | "set_mode" | "set_position" | "set_scale" | "enable"
            | "disable" => vec![("hardware.display.config".to_string(), PermissionOp::Write)],
            _ => vec![("hardware.display".to_string(), PermissionOp::Read)],
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
            "display",
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
        let tool = DisplayConfigTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "list" })),
            vec![("hardware.display".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "test" })),
            vec![("hardware.display".to_string(), PermissionOp::Query)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "set_mode" })),
            vec![("hardware.display.config".to_string(), PermissionOp::Write)]
        );
    }
}
