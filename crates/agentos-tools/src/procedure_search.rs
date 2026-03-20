use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::ProceduralStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ProcedureSearch {
    procedural: Arc<ProceduralStore>,
}

impl ProcedureSearch {
    pub fn new(procedural: Arc<ProceduralStore>) -> Self {
        Self { procedural }
    }
}

#[async_trait]
impl AgentTool for ProcedureSearch {
    fn name(&self) -> &str {
        "procedure-search"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.procedural".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("memory.procedural", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.procedural".to_string(),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("procedure-search requires 'query' field".into())
            })?;

        let top_k = payload
            .get("top_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(20) as usize;

        let min_score = payload
            .get("min_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32;

        // Search globally (no agent_id filter) so agents can discover shared procedures
        let results = self
            .procedural
            .search(query, None, top_k, min_score)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "procedure-search".into(),
                reason: format!("Search failed: {}", e),
            })?;

        let serialized: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.procedure.id,
                    "name": r.procedure.name,
                    "description": r.procedure.description,
                    "preconditions": r.procedure.preconditions,
                    "steps": r.procedure.steps.iter().map(|s| serde_json::json!({
                        "order": s.order,
                        "action": s.action,
                        "tool": s.tool,
                        "expected_outcome": s.expected_outcome,
                    })).collect::<Vec<_>>(),
                    "postconditions": r.procedure.postconditions,
                    "tags": r.procedure.tags,
                    "success_count": r.procedure.success_count,
                    "failure_count": r.procedure.failure_count,
                    "semantic_score": r.semantic_score,
                    "rrf_score": r.rrf_score,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "query": query,
            "count": serialized.len(),
            "results": serialized,
        }))
    }
}
