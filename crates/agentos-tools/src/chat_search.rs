use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Search the agent's own past chat sessions (full-text).
///
/// Kernel-action tool: the chat store lives in the kernel, so this returns a
/// `_kernel_action` envelope for the dispatch loop. Scoping is done kernel-side
/// from the execution context's agent id — an agent can never search another
/// agent's conversations by crafting a payload.
#[derive(Default)]
pub struct ChatSearchTool;

impl ChatSearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentTool for ChatSearchTool {
    fn name(&self) -> &str {
        "chat-search"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.episodic".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("memory.episodic", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.episodic".to_string(),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("chat-search requires a non-empty 'query'".into())
            })?;
        let limit = payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 50);

        Ok(serde_json::json!({
            "_kernel_action": "chat_search",
            "query": query,
            "limit": limit,
        }))
    }
}
