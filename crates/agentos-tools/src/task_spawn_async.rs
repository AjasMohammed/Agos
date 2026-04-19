use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Fire-and-forget async task spawn. Returns a task_id immediately; the parent
/// is never blocked. When the spawned task completes (or fails), its result is
/// automatically injected into the spawner's context window so the agent is
/// notified on its next iteration.
///
/// Use `poll-agent` with the returned task_id to check status at any point.
pub struct TaskSpawnAsyncTool;

impl TaskSpawnAsyncTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TaskSpawnAsyncTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for TaskSpawnAsyncTool {
    fn name(&self) -> &str {
        "task-spawn-async"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.spawn".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let target_agent = payload
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("task-spawn-async requires 'agent' field".into())
            })?;
        let task = payload
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "task-spawn-async requires 'task' field (the prompt for the sub-agent)".into(),
                )
            })?;
        let priority = payload
            .get("priority")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as u8;

        Ok(serde_json::json!({
            "_kernel_action": "spawn_async",
            "target_agent": target_agent,
            "task": task,
            "priority": priority,
        }))
    }
}
