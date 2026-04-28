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
