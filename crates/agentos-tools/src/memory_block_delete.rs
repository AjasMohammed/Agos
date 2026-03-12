use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct MemoryBlockDeleteTool;

impl MemoryBlockDeleteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryBlockDeleteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for MemoryBlockDeleteTool {
    fn name(&self) -> &str {
        "memory-block-delete"
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
                AgentOSError::SchemaValidation("memory-block-delete requires 'label' field".into())
            })?;
        Ok(serde_json::json!({
            "_kernel_action": "memory_block_delete",
            "label": label,
        }))
    }
}
