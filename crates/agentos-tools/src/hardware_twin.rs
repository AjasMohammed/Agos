//! IoT device twin tools: `hardware-get-twin` and `hardware-set-desired`.
//!
//! `hardware-set-desired` is the only agent-facing path for actuating IoT
//! devices, and it is gated by the kernel's `SafetyEngine`: every operator
//! rule in `hardware_limits.toml` is evaluated in Rust before the desired
//! state is written to the twin registry. Both tools fail closed when the
//! twin registry (or, for set-desired, the safety engine) is not attached
//! to the HAL.

use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};

const DEFAULT_DEVICE_TYPE: &str = "iot-device";

fn require_str<'a>(payload: &'a Value, field: &str, tool: &str) -> Result<&'a str, AgentOSError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AgentOSError::ToolExecutionFailed {
            tool_name: tool.to_string(),
            reason: format!("Missing required string field '{field}'"),
        })
}

/// Read the device twin (desired + reported state) for an IoT device.
pub struct HardwareGetTwinTool;

impl HardwareGetTwinTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HardwareGetTwinTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for HardwareGetTwinTool {
    fn name(&self) -> &str {
        "hardware-get-twin"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("hardware.iot".to_string(), PermissionOp::Read)]
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
        let twins = hal
            .twin_registry()
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "Device twin registry is not attached to the HAL".to_string(),
            })?;

        let device_id = require_str(&payload, "device_id", self.name())?;
        match twins.get_twin(device_id).await {
            Some(twin) => {
                serde_json::to_value(&twin).map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: self.name().to_string(),
                    reason: format!("Twin serialization failed: {e}"),
                })
            }
            None => Err(AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: format!(
                    "No twin found for device '{device_id}'. A twin is created the first \
                     time a desired or reported state is recorded for the device."
                ),
            }),
        }
    }
}

/// Set the desired state for an IoT device, subject to operator safety rules.
pub struct HardwareSetDesiredTool;

impl HardwareSetDesiredTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HardwareSetDesiredTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for HardwareSetDesiredTool {
    fn name(&self) -> &str {
        "hardware-set-desired"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("hardware.iot".to_string(), PermissionOp::Write)]
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
        // Fail closed: actuation requires BOTH the twin registry and the
        // safety engine. A missing safety engine must never mean "no rules".
        let twins = hal
            .twin_registry()
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "Device twin registry is not attached to the HAL".to_string(),
            })?;
        let safety = hal
            .safety_engine()
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "Safety engine is not attached to the HAL — refusing to actuate"
                    .to_string(),
            })?;

        let device_id = require_str(&payload, "device_id", self.name())?;
        let desired_state = payload
            .get("desired_state")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "Missing required object field 'desired_state'".to_string(),
            })?;
        // Optional explicit device_type; otherwise reuse the existing twin's
        // type so safety rules keyed on device_type keep matching.
        let device_type = match payload.get("device_type").and_then(Value::as_str) {
            Some(t) if !t.trim().is_empty() => t.to_string(),
            _ => twins
                .get_twin(device_id)
                .await
                .map(|t| t.device_type)
                .unwrap_or_else(|| DEFAULT_DEVICE_TYPE.to_string()),
        };

        if let Err(violation) = safety
            .evaluate(device_id, &device_type, "set_state", &desired_state)
            .await
        {
            let escalation_hint = if violation.requires_escalation {
                " This rule requires operator approval — ask the user to approve the action."
            } else {
                ""
            };
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: format!(
                    "Blocked by safety rule '{}': {}{}",
                    violation.rule_name, violation.message, escalation_hint
                ),
            });
        }

        twins
            .set_desired(
                device_id,
                &device_type,
                desired_state.clone(),
                &context.agent_id,
            )
            .await?;

        let reconciled = twins
            .get_twin(device_id)
            .await
            .map(|t| t.reconciled)
            .unwrap_or(false);

        Ok(json!({
            "device_id": device_id,
            "device_type": device_type,
            "desired_state": desired_state,
            "reconciled": reconciled,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_hal::safety::{SafetyCondition, SafetyEngine, SafetyRule};
    use agentos_hal::twin::TwinRegistry;
    use agentos_hal::HardwareAbstractionLayer;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_context(
        data_dir: &std::path::Path,
        hal: Option<Arc<HardwareAbstractionLayer>>,
    ) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal,
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
        }
    }

    fn hal_with(twins: Arc<TwinRegistry>, rules: Vec<SafetyRule>) -> Arc<HardwareAbstractionLayer> {
        Arc::new(
            HardwareAbstractionLayer::new()
                .with_twin_registry(Arc::clone(&twins))
                .with_safety_engine(Arc::new(SafetyEngine::with_rules(rules, twins))),
        )
    }

    fn blocking_rule(escalation: bool) -> SafetyRule {
        SafetyRule {
            name: "thermal".into(),
            description: "Block heater when hot".into(),
            device_type: "heater".into(),
            action: "set_state".into(),
            condition: SafetyCondition::ReportedThreshold {
                sensor_device_id: "sensor_1".into(),
                field: "temperature".into(),
                op: "lt".into(),
                threshold: 75.0,
            },
            error_message: "Temperature too high".into(),
            escalation,
        }
    }

    #[tokio::test]
    async fn set_desired_writes_twin_when_rules_pass() {
        let tmp = TempDir::new().unwrap();
        let twins = Arc::new(TwinRegistry::new(&tmp.path().join("twins.db")).unwrap());
        let hal = hal_with(Arc::clone(&twins), vec![]);

        let result = HardwareSetDesiredTool::new()
            .execute(
                json!({"device_id": "light.kitchen", "device_type": "light",
                       "desired_state": {"on": true}}),
                make_context(tmp.path(), Some(hal)),
            )
            .await
            .unwrap();

        assert_eq!(result["device_id"], "light.kitchen");
        let twin = twins.get_twin("light.kitchen").await.unwrap();
        assert_eq!(twin.desired_state, json!({"on": true}));
        assert_eq!(twin.device_type, "light");
    }

    #[tokio::test]
    async fn set_desired_blocked_by_safety_rule() {
        let tmp = TempDir::new().unwrap();
        let twins = Arc::new(TwinRegistry::new(&tmp.path().join("twins.db")).unwrap());
        twins
            .update_reported("sensor_1", "sensor", json!({"temperature": 90.0}))
            .await
            .unwrap();
        let hal = hal_with(Arc::clone(&twins), vec![blocking_rule(true)]);

        let err = HardwareSetDesiredTool::new()
            .execute(
                json!({"device_id": "heater_1", "device_type": "heater",
                       "desired_state": {"on": true}}),
                make_context(tmp.path(), Some(hal)),
            )
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("thermal"), "missing rule name: {msg}");
        assert!(
            msg.contains("Temperature too high"),
            "missing message: {msg}"
        );
        assert!(msg.contains("operator approval"), "missing hint: {msg}");
        // Blocked action must not write the twin.
        assert!(twins.get_twin("heater_1").await.is_none());
    }

    #[tokio::test]
    async fn set_desired_fails_closed_without_safety_engine() {
        let tmp = TempDir::new().unwrap();
        let twins = Arc::new(TwinRegistry::new(&tmp.path().join("twins.db")).unwrap());
        let hal = Arc::new(HardwareAbstractionLayer::new().with_twin_registry(twins));

        let err = HardwareSetDesiredTool::new()
            .execute(
                json!({"device_id": "d", "desired_state": {"on": true}}),
                make_context(tmp.path(), Some(hal)),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Safety engine"));
    }

    #[tokio::test]
    async fn set_desired_reuses_existing_twin_device_type() {
        let tmp = TempDir::new().unwrap();
        let twins = Arc::new(TwinRegistry::new(&tmp.path().join("twins.db")).unwrap());
        twins
            .update_reported("heater_1", "heater", json!({"on": false}))
            .await
            .unwrap();
        twins
            .update_reported("sensor_1", "sensor", json!({"temperature": 90.0}))
            .await
            .unwrap();
        // Rule keys on device_type = "heater"; payload omits device_type, so the
        // tool must pick it up from the existing twin for the rule to fire.
        let hal = hal_with(Arc::clone(&twins), vec![blocking_rule(false)]);

        let err = HardwareSetDesiredTool::new()
            .execute(
                json!({"device_id": "heater_1", "desired_state": {"on": true}}),
                make_context(tmp.path(), Some(hal)),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("thermal"));
    }

    #[tokio::test]
    async fn get_twin_roundtrip_and_missing() {
        let tmp = TempDir::new().unwrap();
        let twins = Arc::new(TwinRegistry::new(&tmp.path().join("twins.db")).unwrap());
        twins
            .update_reported("sensor.temp", "sensor", json!({"temperature": 21.5}))
            .await
            .unwrap();
        let hal = hal_with(twins, vec![]);

        let twin = HardwareGetTwinTool::new()
            .execute(
                json!({"device_id": "sensor.temp"}),
                make_context(tmp.path(), Some(Arc::clone(&hal))),
            )
            .await
            .unwrap();
        assert_eq!(twin["reported_state"], json!({"temperature": 21.5}));

        let err = HardwareGetTwinTool::new()
            .execute(
                json!({"device_id": "ghost"}),
                make_context(tmp.path(), Some(hal)),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No twin found"));
    }

    #[tokio::test]
    async fn tools_fail_closed_without_twin_registry() {
        let tmp = TempDir::new().unwrap();
        let hal = Arc::new(HardwareAbstractionLayer::new());

        let err = HardwareGetTwinTool::new()
            .execute(
                json!({"device_id": "d"}),
                make_context(tmp.path(), Some(Arc::clone(&hal))),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("twin registry"));

        let err = HardwareSetDesiredTool::new()
            .execute(
                json!({"device_id": "d", "desired_state": {}}),
                make_context(tmp.path(), Some(hal)),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("twin registry"));
    }
}
