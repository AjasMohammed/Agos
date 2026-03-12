use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct MemoryBlockReadTool;

impl MemoryBlockReadTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryBlockReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for MemoryBlockReadTool {
    fn name(&self) -> &str {
        "memory-block-read"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.blocks".to_string(), PermissionOp::Read)]
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
                AgentOSError::SchemaValidation("memory-block-read requires 'label' field".into())
            })?;
        Ok(serde_json::json!({
            "_kernel_action": "memory_block_read",
            "label": label,
        }))
    }
}
