use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
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
            "capture" | "burst" => {
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

        // Consent grants are operator-originated (`agentos hal approve`);
        // an agent must never grant or revoke its own capture consent.
        if let Some(action) = payload.get("action").and_then(Value::as_str) {
            if matches!(action, "grant_capture_consent" | "revoke_capture_consent") {
                return Err(AgentOSError::PermissionDenied {
                    resource: "hardware.webcam.capture.consent".to_string(),
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
            "webcam",
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
    use agentos_hal::{HalDriver, HardwareAbstractionLayer};
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

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

    /// Records the payload that reaches the driver, standing in for the real
    /// webcam driver so the test can observe what the tool wrapper forwarded.
    struct RecordingDriver {
        seen: Arc<Mutex<Vec<Value>>>,
    }

    #[async_trait]
    impl HalDriver for RecordingDriver {
        fn name(&self) -> &str {
            "webcam"
        }

        fn required_permission(&self) -> (&str, PermissionOp) {
            ("hardware.webcam.capture", PermissionOp::Execute)
        }

        async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
            self.seen.lock().unwrap().push(params);
            Ok(json!({ "ok": true }))
        }
    }

    fn make_context(hal: Arc<HardwareAbstractionLayer>) -> (ToolExecutionContext, AgentID) {
        let agent_id = AgentID::new();
        // The tool forwards the agent's real grant to hal.query, so the test
        // context must actually hold the capture permission.
        let mut permissions = PermissionSet::new();
        permissions.grant_op(
            "hardware.webcam.capture".to_string(),
            PermissionOp::Execute,
            None,
        );
        let context = ToolExecutionContext {
            data_dir: std::env::temp_dir(),
            task_id: TaskID::new(),
            agent_id,
            trace_id: TraceID::new(),
            permissions,
            vault: None,
            hal: Some(hal),
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
        };
        (context, agent_id)
    }

    #[tokio::test]
    async fn identity_is_injected_and_spoofed_claims_are_stripped() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(RecordingDriver { seen: seen.clone() }));
        let (context, agent_id) = make_context(Arc::new(hal));

        WebcamTool::new()
            .execute(
                json!({
                    "action": "capture",
                    "device": "/dev/video0",
                    "agent_id": "spoofed",
                    "session_id": "spoofed-session",
                    "__authenticated_agent_id": "forged",
                }),
                context,
            )
            .await
            .expect("mock capture should succeed");

        let payloads = seen.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        let forwarded = &payloads[0];
        assert_eq!(
            forwarded["__authenticated_agent_id"],
            agent_id.to_string(),
            "reserved key must carry the kernel identity, not the forged one"
        );
        assert!(forwarded.get("agent_id").is_none());
        assert!(forwarded.get("session_id").is_none());
    }

    #[tokio::test]
    async fn agent_invoked_consent_grant_is_rejected_before_the_hal() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(RecordingDriver { seen: seen.clone() }));
        let (context, _) = make_context(Arc::new(hal));

        let err = WebcamTool::new()
            .execute(
                json!({ "action": "grant_capture_consent", "device": "/dev/video0" }),
                context,
            )
            .await
            .expect_err("agent-invoked grant must be rejected");

        match err {
            AgentOSError::PermissionDenied { operation, .. } => {
                assert_eq!(operation, "operator_approval_required");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(seen.lock().unwrap().is_empty(), "must never reach the HAL");
    }
}
