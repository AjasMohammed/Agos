use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp, PermissionSet};
use async_trait::async_trait;
use serde_json::Value;

pub struct WebcamTool;

impl WebcamTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebcamTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for WebcamTool {
    fn name(&self) -> &str {
        "webcam"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("hardware.webcam.list".to_string(), PermissionOp::Read),
            ("hardware.webcam.capture".to_string(), PermissionOp::Read),
            ("hardware.webcam.capture".to_string(), PermissionOp::Execute),
        ]
    }

    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        match payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => vec![("hardware.webcam.list".to_string(), PermissionOp::Read)],
            "capture" | "burst" | "grant_capture_consent" | "revoke_capture_consent" => {
                vec![("hardware.webcam.capture".to_string(), PermissionOp::Execute)]
            }
            "list_capture_consents" => {
                vec![("hardware.webcam.capture".to_string(), PermissionOp::Read)]
            }
            _ => vec![("hardware.webcam.list".to_string(), PermissionOp::Read)],
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
            "webcam",
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
        let tool = WebcamTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "list" })),
            vec![("hardware.webcam.list".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "capture" })),
            vec![("hardware.webcam.capture".to_string(), PermissionOp::Execute)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "list_capture_consents" })),
            vec![("hardware.webcam.capture".to_string(), PermissionOp::Read)]
        );
    }
}
