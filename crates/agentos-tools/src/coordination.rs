use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Spawn a sub-agent to handle a specific subtask.
/// Uses the `_kernel_action: "spawn_agent"` pattern so the kernel intercepts
/// the result and performs the privileged spawn operation.
pub struct SpawnAgentTool;

impl SpawnAgentTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SpawnAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn-agent"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.spawn".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let agent = payload
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("spawn-agent requires 'agent' field".into())
            })?;
        let prompt = payload
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("spawn-agent requires 'prompt' field".into())
            })?;

        let permissions: Vec<serde_json::Value> = payload
            .get("permissions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Clamp context_messages to the schema maximum (100) to prevent
        // excessive context cloning even if schema validation is bypassed.
        let context_messages = payload
            .get("context_messages")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(100);

        tracing::debug!(
            agent = %agent,
            permissions_count = permissions.len(),
            context_messages = context_messages,
            "spawn_agent tool: forwarding to kernel"
        );

        Ok(serde_json::json!({
            "_kernel_action": "spawn_agent",
            "agent": agent,
            "prompt": prompt,
            "permissions": permissions,
            "context_messages": context_messages,
        }))
    }
}

/// Wait for spawned sub-agents to complete and retrieve their results.
/// Uses the `_kernel_action: "await_agents"` pattern.
pub struct AwaitAgentsTool;

impl AwaitAgentsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AwaitAgentsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AwaitAgentsTool {
    fn name(&self) -> &str {
        "await-agents"
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
                AgentOSError::SchemaValidation("await-agents requires 'task_ids' array".into())
            })?;

        if task_ids.is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "await-agents: task_ids must not be empty".into(),
            ));
        }

        tracing::debug!(
            task_count = task_ids.len(),
            "await_agents tool: forwarding to kernel"
        );

        Ok(serde_json::json!({
            "_kernel_action": "await_agents",
            "task_ids": task_ids,
        }))
    }
}
