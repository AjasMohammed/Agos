use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct GetTaskLogsTool;

impl GetTaskLogsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GetTaskLogsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for GetTaskLogsTool {
    fn name(&self) -> &str {
        "get-task-logs"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let run_id = payload
            .get("run_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::KernelError {
                reason: "run_id is required".into(),
            })?
            .to_string();
        Ok(serde_json::json!({
            "_kernel_action": "get_task_logs",
            "run_id": run_id,
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
    async fn emits_kernel_action_with_run_id() {
        let out = GetTaskLogsTool::new()
            .execute(serde_json::json!({"run_id": "abc-123"}), ctx())
            .await
            .unwrap();
        assert_eq!(out["_kernel_action"], "get_task_logs");
        assert_eq!(out["run_id"], "abc-123");
    }

    #[tokio::test]
    async fn missing_run_id_is_error() {
        let err = GetTaskLogsTool::new()
            .execute(serde_json::json!({}), ctx())
            .await;
        assert!(err.is_err());
    }
}
