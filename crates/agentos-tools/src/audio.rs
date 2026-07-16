use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct AudioTool;

impl AudioTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AudioTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AudioTool {
    fn name(&self) -> &str {
        "audio"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("hardware.audio.list".to_string(), PermissionOp::Read),
            ("hardware.audio.capture".to_string(), PermissionOp::Read),
            ("hardware.audio.capture".to_string(), PermissionOp::Execute),
            ("hardware.audio.playback".to_string(), PermissionOp::Execute),
            ("hardware.audio.volume".to_string(), PermissionOp::Read),
            ("hardware.audio.volume".to_string(), PermissionOp::Write),
        ]
    }

    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        match payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => vec![("hardware.audio.list".to_string(), PermissionOp::Read)],
            "capture" => {
                vec![("hardware.audio.capture".to_string(), PermissionOp::Execute)]
            }
            "list_capture_consents" => {
                vec![("hardware.audio.capture".to_string(), PermissionOp::Read)]
            }
            "playback" => vec![("hardware.audio.playback".to_string(), PermissionOp::Execute)],
            "volume" => {
                let op = if payload.get("volume").is_some() {
                    PermissionOp::Write
                } else {
                    PermissionOp::Read
                };
                vec![("hardware.audio.volume".to_string(), op)]
            }
            _ => vec![("hardware.audio.list".to_string(), PermissionOp::Read)],
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

        // Consent grants are operator-originated (`agentos hal approve`);
        // an agent must never grant or revoke its own capture consent.
        if let Some(action) = payload.get("action").and_then(Value::as_str) {
            if matches!(action, "grant_capture_consent" | "revoke_capture_consent") {
                return Err(AgentOSError::PermissionDenied {
                    resource: "hardware.audio.capture.consent".to_string(),
                    operation: "operator_approval_required".to_string(),
                });
            }
        }

        // Stamp the authenticated identity into the payload under a reserved
        // key the driver trusts, and strip every agent-supplied identity claim
        // (including an attempt to forge the reserved key itself).
        let mut payload = payload;
        if let Value::Object(map) = &mut payload {
            map.remove("agent_id");
            map.remove("session_id");
            map.insert(
                "__authenticated_agent_id".to_string(),
                Value::String(context.agent_id.to_string()),
            );
        }

        // Forward the agent's real grant — the kernel validated the token
        // against the payload-scoped permissions, so the HAL-internal check
        // re-verifies the same authority instead of a self-minted set.
        hal.query(
            "audio",
            payload,
            &context.permissions,
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
        let tool = AudioTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "list" })),
            vec![("hardware.audio.list".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "capture" })),
            vec![("hardware.audio.capture".to_string(), PermissionOp::Execute)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "volume" })),
            vec![("hardware.audio.volume".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "volume", "volume": 0.5 })),
            vec![("hardware.audio.volume".to_string(), PermissionOp::Write)]
        );
    }
}
