use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::SemanticStore;
use agentos_types::*;
use async_trait::async_trait;
use std::sync::Arc;

pub struct ArchivalSearch {
    semantic: Arc<SemanticStore>,
}

impl ArchivalSearch {
    pub fn new(semantic: Arc<SemanticStore>) -> Self {
        Self { semantic }
    }
}

#[async_trait]
impl AgentTool for ArchivalSearch {
    fn name(&self) -> &str {
        "archival-search"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("archival-search requires 'query'".into())
            })?;
        let top_k = payload.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let results = self
            .semantic
            .search(query, Some(&context.agent_id), top_k, 0.0)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "archival-search".to_string(),
                reason: e.to_string(),
            })?;
        Ok(serde_json::json!({
            "count": results.len(),
            "results": results.into_iter().map(|r| serde_json::json!({
                "key": r.entry.key,
                "content": r.chunk.content,
                "score": r.rrf_score,
            })).collect::<Vec<_>>(),
        }))
    }
}
