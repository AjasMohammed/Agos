use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct ScheduleControlTool;

impl ScheduleControlTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ScheduleControlTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ScheduleControlTool {
    fn name(&self) -> &str {
        "schedule-control"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("schedule.job".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let action = payload
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("schedule-control requires 'action'".into())
            })?;
        if !matches!(action, "pause" | "resume" | "delete") {
            return Err(AgentOSError::SchemaValidation(
                "schedule-control 'action' must be one of: pause, resume, delete".into(),
            ));
        }
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("schedule-control requires 'name'".into())
            })?
            .to_string();
        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "schedule-control 'name' must be 1-128 characters".into(),
            ));
        }

        Ok(serde_json::json!({
            "_kernel_action": "control_schedule",
            "action": action,
            "name": name,
        }))
    }
}
