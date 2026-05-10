use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct GetScheduleRunsTool;

impl GetScheduleRunsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GetScheduleRunsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for GetScheduleRunsTool {
    fn name(&self) -> &str {
        "get-schedule-runs"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let schedule_id = payload
            .get("schedule_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::KernelError {
                reason: "schedule_id is required".into(),
            })?
            .to_string();
        let limit = payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 100);
        let state = payload
            .get("state")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok(serde_json::json!({
            "_kernel_action": "get_schedule_runs",
            "schedule_id": schedule_id,
            "limit": limit,
            "state": state,
        }))
    }
}
