use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::ProceduralStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ProcedureList {
    procedural: Arc<ProceduralStore>,
}

impl ProcedureList {
    pub fn new(procedural: Arc<ProceduralStore>) -> Self {
        Self { procedural }
    }
}

#[async_trait]
impl AgentTool for ProcedureList {
    fn name(&self) -> &str {
        "procedure-list"
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

        const MAX_LIMIT: usize = 100;
        let limit = payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(MAX_LIMIT as u64) as usize;

        // List procedures owned by this agent (plus shared/global ones)
        let procedures = self
            .procedural
            .list_by_agent(Some(&context.agent_id), limit)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "procedure-list".into(),
                reason: format!("List query failed: {}", e),
            })?;

        let rows: Vec<serde_json::Value> = procedures
            .into_iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "description": p.description,
                    "tags": p.tags,
                    "step_count": p.steps.len(),
                    "success_count": p.success_count,
                    "failure_count": p.failure_count,
                    "updated_at": p.updated_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "count": rows.len(),
            "procedures": rows,
        }))
    }
}
