use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct MemoryBlockListTool;

impl MemoryBlockListTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryBlockListTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for MemoryBlockListTool {
    fn name(&self) -> &str {
        "memory-block-list"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.blocks".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "_kernel_action": "memory_block_list",
        }))
    }
}
