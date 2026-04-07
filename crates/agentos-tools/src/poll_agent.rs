use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Non-blocking tool to check the status and progress of spawned sub-agents.
///
/// Returns current state, iteration count, and recent progress summaries
/// without blocking the parent task. Uses the `_kernel_action: "poll_agents"`
/// pattern so the kernel handles the privileged task store lookup.
pub struct PollAgentTool;

impl PollAgentTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PollAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for PollAgentTool {
    fn name(&self) -> &str {
        "poll-agent"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.spawn".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let task_ids: Vec<serde_json::Value> = payload
            .get("task_ids")
            .and_then(|v| v.as_array())
            .cloned()
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("poll-agent requires 'task_ids' array".into())
            })?;

        if task_ids.is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "poll-agent: task_ids must not be empty".into(),
            ));
        }

        let include_progress = payload
            .get("include_progress")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        tracing::debug!(
            task_count = task_ids.len(),
            include_progress = include_progress,
            "poll_agent tool: forwarding to kernel"
        );

        Ok(serde_json::json!({
            "_kernel_action": "poll_agents",
            "task_ids": task_ids,
            "include_progress": include_progress,
        }))
    }
}
