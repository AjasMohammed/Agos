use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct ProcessManagerTool;

impl ProcessManagerTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProcessManagerTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ProcessManagerTool {
    fn name(&self) -> &str {
        "process-manager"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("process.list".to_string(), PermissionOp::Read),
            ("process.kill".to_string(), PermissionOp::Execute),
        ]
    }

    /// The runner checks *every* entry of the returned list, so without this
    /// override a read-only `list` demanded `process.kill` as well and an agent
    /// holding only `process.list` was denied outright. `execute` below already
    /// scopes the HAL grant per action; this makes the gate agree with it.
    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        match payload.get("action").and_then(Value::as_str) {
            // A missing action defaults to `list` in `execute`.
            None | Some("list") => vec![("process.list".to_string(), PermissionOp::Read)],
            Some("kill") => vec![("process.kill".to_string(), PermissionOp::Execute)],
            // An action this gate does not recognise is charged the most
            // expensive known permission, not the cheapest. `execute` rejects
            // unknown actions today, so nothing is lost — but if a `signal` or
            // `suspend` arm is added there later, a fall-through to
            // `process.list` would gate it at read.
            Some(_) => vec![("process.kill".to_string(), PermissionOp::Execute)],
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

        let action = payload
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("list");

        let mut perms = agentos_types::PermissionSet::new();
        match action {
            "list" => {
                perms.grant("process.list".to_string(), true, false, false, None);
            }
            "kill" => {
                perms.grant("process.kill".to_string(), false, false, true, None);
            }
            _ => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Unsupported process-manager action: '{}'. Valid actions: 'list', 'kill'",
                    action
                )));
            }
        }

        hal.query(
            "process",
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

    /// The runner checks every returned entry, so a read-only `list` that
    /// reported `process.kill` was denied outright for agents holding only
    /// `process.list` (observed 2026-09-08).
    #[test]
    fn list_does_not_require_kill() {
        let tool = ProcessManagerTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({"action": "list", "limit": 20})),
            vec![("process.list".to_string(), PermissionOp::Read)]
        );
    }

    #[test]
    fn kill_requires_kill() {
        let tool = ProcessManagerTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({"action": "kill", "pid": 1})),
            vec![("process.kill".to_string(), PermissionOp::Execute)]
        );
    }

    /// A missing action defaults to `list` in `execute`, so the gate agrees.
    #[test]
    fn missing_action_requires_only_list() {
        let tool = ProcessManagerTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({})),
            vec![("process.list".to_string(), PermissionOp::Read)]
        );
    }

    /// Fail closed: an action the gate does not know is charged the most
    /// expensive permission, so adding a new arm to `execute` can never
    /// silently ship it at read-level.
    #[test]
    fn unknown_action_is_charged_the_strictest_permission() {
        let tool = ProcessManagerTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({"action": "signal"})),
            vec![("process.kill".to_string(), PermissionOp::Execute)]
        );
    }
}
