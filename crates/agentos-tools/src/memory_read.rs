use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::SemanticStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct MemoryRead {
    semantic: Arc<SemanticStore>,
}

impl MemoryRead {
    pub fn new(semantic: Arc<SemanticStore>) -> Self {
        Self { semantic }
    }
}

#[async_trait]
impl AgentTool for MemoryRead {
    fn name(&self) -> &str {
        "memory-read"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("memory.semantic", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.semantic".to_string(),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        let key = payload.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
            AgentOSError::SchemaValidation("memory-read requires 'key' field".into())
        })?;

        let entry =
            self.semantic
                .get_by_key(key)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-read".into(),
                    reason: format!("Read failed: {}", e),
                })?;

        match entry {
            Some(e) => Ok(serde_json::json!({
                "found": true,
                "id": e.id,
                "key": e.key,
                "content": e.full_content,
                "tags": e.tags,
                "created_at": e.created_at.to_rfc3339(),
                "updated_at": e.updated_at.to_rfc3339(),
            })),
            None => Ok(serde_json::json!({
                "found": false,
                "key": key,
                "message": format!("No semantic memory entry found for key '{}'", key),
            })),
        }
    }
}
