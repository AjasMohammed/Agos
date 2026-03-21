use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::{EpisodicStore, SemanticStore};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct MemoryRead {
    semantic: Arc<SemanticStore>,
    episodic: Arc<EpisodicStore>,
}

impl MemoryRead {
    pub fn new(semantic: Arc<SemanticStore>, episodic: Arc<EpisodicStore>) -> Self {
        Self { semantic, episodic }
    }
}

#[async_trait]
impl AgentTool for MemoryRead {
    fn name(&self) -> &str {
        "memory-read"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("memory.semantic".to_string(), PermissionOp::Read),
            ("memory.episodic".to_string(), PermissionOp::Read),
        ]
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

        match scope {
            "episodic" => {
                if !context
                    .permissions
                    .check("memory.episodic", PermissionOp::Read)
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "memory.episodic".to_string(),
                        operation: format!("{:?}", PermissionOp::Read),
                    });
                }

                let id = payload.get("id").and_then(|v| v.as_i64()).ok_or_else(|| {
                    AgentOSError::SchemaValidation(
                        "memory-read with scope='episodic' requires integer 'id' field".into(),
                    )
                })?;

                let entry = self.episodic.get_by_id(id).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "memory-read".into(),
                        reason: format!("Episodic read failed: {}", e),
                    }
                })?;

                match entry {
                    Some(e) => {
                        // Enforce agent-scoping: agents can only read their own episodic entries
                        if e.agent_id != context.agent_id {
                            return Ok(serde_json::json!({
                                "found": false,
                                "scope": "episodic",
                                "id": id,
                                "message": format!("No episodic entry found with id {}", id),
                            }));
                        }
                        Ok(serde_json::json!({
                            "found": true,
                            "scope": "episodic",
                            "id": e.id,
                            "task_id": e.task_id.as_uuid().to_string(),
                            "agent_id": e.agent_id.as_uuid().to_string(),
                            "entry_type": format!("{:?}", e.entry_type),
                            "content": e.content,
                            "summary": e.summary,
                            "metadata": e.metadata,
                            "timestamp": e.timestamp.to_rfc3339(),
                        }))
                    }
                    None => Ok(serde_json::json!({
                        "found": false,
                        "scope": "episodic",
                        "id": id,
                        "message": format!("No episodic entry found with id {}", id),
                    })),
                }
            }
            "semantic" => {
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
                    AgentOSError::SchemaValidation(
                        "memory-read requires 'key' field for semantic scope".into(),
                    )
                })?;

                let entry = self.semantic.get_by_key(key).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "memory-read".into(),
                        reason: format!("Read failed: {}", e),
                    }
                })?;

                match entry {
                    Some(e) => Ok(serde_json::json!({
                        "found": true,
                        "scope": "semantic",
                        "id": e.id,
                        "key": e.key,
                        "content": e.full_content,
                        "tags": e.tags,
                        "created_at": e.created_at.to_rfc3339(),
                        "updated_at": e.updated_at.to_rfc3339(),
                    })),
                    None => Ok(serde_json::json!({
                        "found": false,
                        "scope": "semantic",
                        "key": key,
                        "message": format!("No semantic memory entry found for key '{}'", key),
                    })),
                }
            }
            other => Err(AgentOSError::SchemaValidation(format!(
                "Unknown memory scope '{}'. Valid values: 'semantic', 'episodic'",
                other
            ))),
        }
    }
}
