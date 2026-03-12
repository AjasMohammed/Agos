use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct MemoryBlockWriteTool;

impl MemoryBlockWriteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryBlockWriteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for MemoryBlockWriteTool {
    fn name(&self) -> &str {
        "memory-block-write"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.blocks".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let label = payload
            .get("label")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("memory-block-write requires 'label' field".into())
            })?;
        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("memory-block-write requires 'content' field".into())
            })?;
        Ok(serde_json::json!({
            "_kernel_action": "memory_block_write",
            "label": label,
            "content": content,
        }))
    }
}
