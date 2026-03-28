use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_scratch::ScratchpadStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ScratchSearchTool {
    store: Arc<ScratchpadStore>,
}

impl ScratchSearchTool {
    pub fn new(store: Arc<ScratchpadStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for ScratchSearchTool {
    fn name(&self) -> &str {
        "scratch-search"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("scratchpad".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("scratchpad", PermissionOp::Read) {
            return Err(AgentOSError::PermissionDenied {
                resource: "scratchpad".to_string(),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "scratch-search requires 'query' field (string)".into(),
                )
            })?;

        let tags: Vec<String> = match payload.get("tags") {
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        };

        let limit = payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(100) as usize;

        // Optional cross-agent search: "agent_id" field specifies the target agent
        let target_agent = payload.get("agent_id").and_then(|v| v.as_str());

        let effective_agent_id = if let Some(target) = target_agent {
            // Cross-agent search requires scratch.cross:<target> permission
            let cross_resource = format!("scratch.cross:{}", target);
            if !context
                .permissions
                .check(&cross_resource, PermissionOp::Read)
            {
                return Err(AgentOSError::PermissionDenied {
                    resource: cross_resource,
                    operation: format!("{:?}", PermissionOp::Read),
                });
            }
            target.to_string()
        } else {
            context.agent_id.to_string()
        };

        let results = self
            .store
            .search(&effective_agent_id, query, &tags, limit)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "scratch-search".into(),
                reason: format!("Search failed: {}", e),
            })?;

        let items: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "page_id": r.page.id,
                    "title": r.page.title,
                    "agent_id": r.page.agent_id,
                    "snippet": r.snippet,
                    "rank": r.rank,
                    "tags": r.page.tags,
                    "updated_at": r.page.updated_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "count": items.len(),
            "results": items,
        }))
    }
}
