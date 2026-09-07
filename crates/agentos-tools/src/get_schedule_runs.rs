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

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::path::PathBuf;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
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

    #[tokio::test]
    async fn emits_action_and_clamps_limit() {
        let out = GetScheduleRunsTool::new()
            .execute(
                serde_json::json!({"schedule_id": "nightly", "limit": 9999}),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(out["_kernel_action"], "get_schedule_runs");
        assert_eq!(out["schedule_id"], "nightly");
        assert_eq!(out["limit"], 100); // clamped to max
    }

    #[tokio::test]
    async fn missing_schedule_id_is_error() {
        let err = GetScheduleRunsTool::new()
            .execute(serde_json::json!({}), ctx())
            .await;
        assert!(err.is_err());
    }
}
