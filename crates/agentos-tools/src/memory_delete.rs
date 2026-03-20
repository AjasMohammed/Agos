use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::{EpisodicStore, SemanticStore};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct MemoryDelete {
    semantic: Arc<SemanticStore>,
    episodic: Arc<EpisodicStore>,
}

impl MemoryDelete {
    pub fn new(semantic: Arc<SemanticStore>, episodic: Arc<EpisodicStore>) -> Self {
        Self { semantic, episodic }
    }
}

#[async_trait]
impl AgentTool for MemoryDelete {
    fn name(&self) -> &str {
        "memory-delete"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Scope-aware checks enforced inside execute()
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let scope = payload
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("semantic");

        if scope == "episodic" {
            if !context
                .permissions
                .check("memory.episodic", PermissionOp::Write)
            {
                return Err(AgentOSError::PermissionDenied {
                    resource: "memory.episodic".to_string(),
                    operation: format!("{:?}", PermissionOp::Write),
                });
            }

            let id = payload.get("id").and_then(|v| v.as_i64()).ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "memory-delete requires numeric 'id' for episodic scope".into(),
                )
            })?;

            self.episodic
                .delete(id)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-delete".into(),
                    reason: format!("Episodic delete failed: {}", e),
                })?;

            Ok(serde_json::json!({
                "success": true,
                "scope": "episodic",
                "deleted_id": id,
                "message": "Episodic memory entry deleted",
            }))
        } else {
            if !context
                .permissions
                .check("memory.semantic", PermissionOp::Write)
            {
                return Err(AgentOSError::PermissionDenied {
                    resource: "memory.semantic".to_string(),
                    operation: format!("{:?}", PermissionOp::Write),
                });
            }

            let id = payload.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "memory-delete requires string 'id' for semantic scope".into(),
                )
            })?;

            self.semantic
                .delete(id)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-delete".into(),
                    reason: format!("Semantic delete failed: {}", e),
                })?;

            Ok(serde_json::json!({
                "success": true,
                "scope": "semantic",
                "deleted_id": id,
                "message": "Semantic memory entry deleted",
            }))
        }
    }
}
