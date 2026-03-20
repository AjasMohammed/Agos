use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::ProceduralStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ProcedureDelete {
    procedural: Arc<ProceduralStore>,
}

impl ProcedureDelete {
    pub fn new(procedural: Arc<ProceduralStore>) -> Self {
        Self { procedural }
    }
}

#[async_trait]
impl AgentTool for ProcedureDelete {
    fn name(&self) -> &str {
        "procedure-delete"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.procedural".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("memory.procedural", PermissionOp::Write)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.procedural".to_string(),
                operation: format!("{:?}", PermissionOp::Write),
            });
        }

        let id = payload.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
            AgentOSError::SchemaValidation("procedure-delete requires 'id' field".into())
        })?;

        self.procedural
            .delete(id)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "procedure-delete".into(),
                reason: format!("Delete failed: {}", e),
            })?;

        Ok(serde_json::json!({
            "success": true,
            "deleted_id": id,
            "message": "Procedure deleted from procedural memory",
        }))
    }
}
