use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::EpisodicStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct EpisodicList {
    episodic: Arc<EpisodicStore>,
}

impl EpisodicList {
    pub fn new(episodic: Arc<EpisodicStore>) -> Self {
        Self { episodic }
    }
}

#[async_trait]
impl AgentTool for EpisodicList {
    fn name(&self) -> &str {
        "episodic-list"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // own-task access is free; cross-task requires memory.episodic:r
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        const MAX_LIMIT: u32 = 200;
        let limit = payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(MAX_LIMIT as u64) as u32;

        // Default to current task timeline; cross-task requires permission
        let entries = self
            .episodic
            .timeline_by_task(&context.task_id, limit)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "episodic-list".into(),
                reason: format!("Timeline query failed: {}", e),
            })?;

        let rows: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|ep| {
                serde_json::json!({
                    "id": ep.id,
                    "task_id": ep.task_id.to_string(),
                    "agent_id": ep.agent_id.to_string(),
                    "entry_type": ep.entry_type.as_str(),
                    "content": ep.content,
                    "summary": ep.summary,
                    "timestamp": ep.timestamp.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "task_id": context.task_id.to_string(),
            "count": rows.len(),
            "entries": rows,
        }))
    }
}
