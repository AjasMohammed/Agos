use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::{EpisodicStore, SemanticStore};
use agentos_types::*;
use async_trait::async_trait;
use std::sync::Arc;

pub struct MemoryWrite {
    semantic: Arc<SemanticStore>,
    episodic: Arc<EpisodicStore>,
}

impl MemoryWrite {
    pub fn new(semantic: Arc<SemanticStore>, episodic: Arc<EpisodicStore>) -> Self {
        Self { semantic, episodic }
    }
}

#[async_trait]
impl AgentTool for MemoryWrite {
    fn name(&self) -> &str {
        "memory-write"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Scope-aware checks are enforced inside execute():
        // - semantic -> memory.semantic:w
        // - episodic -> memory.episodic:w
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("memory-write requires 'content' field".into())
            })?;

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

            let summary = payload.get("summary").and_then(|v| v.as_str());
            self.episodic
                .record(
                    &context.task_id,
                    &context.agent_id,
                    agentos_memory::EpisodeType::SystemEvent,
                    content,
                    summary,
                    None,
                    &context.trace_id,
                )
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-write".into(),
                    reason: format!("Episodic write failed: {}", e),
                })?;

            Ok(serde_json::json!({
                "success": true,
                "scope": "episodic",
                "message": "Episodic memory entry stored",
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

            // Auto-generate key from first few words if not provided
            let auto_key: String = content
                .split_whitespace()
                .take(6)
                .collect::<Vec<_>>()
                .join(" ");
            let key = payload
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or(&auto_key);

            let tags: Vec<&str> = match payload.get("tags") {
                Some(serde_json::Value::Array(values)) => {
                    values.iter().filter_map(|v| v.as_str()).collect()
                }
                Some(serde_json::Value::String(s)) => s
                    .split(',')
                    .map(|t| t.trim())
                    .filter(|t| !t.is_empty())
                    .collect(),
                _ => Vec::new(),
            };

            let id = self
                .semantic
                .write(key, content, Some(&context.agent_id), &tags)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-write".into(),
                    reason: format!("Semantic write failed: {}", e),
                })?;

            Ok(serde_json::json!({
                "success": true,
                "scope": "semantic",
                "id": id,
                "message": "Semantic memory entry stored with embedding",
            }))
        }
    }
}
