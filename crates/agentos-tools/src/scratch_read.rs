use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_scratch::{parse_page_ref, PageRef, ScratchpadStore};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ScratchReadTool {
    store: Arc<ScratchpadStore>,
}

impl ScratchReadTool {
    pub fn new(store: Arc<ScratchpadStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for ScratchReadTool {
    fn name(&self) -> &str {
        "scratch-read"
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

        let raw_title = payload
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "scratch-read requires 'title' field (string)".into(),
                )
            })?;

        // Parse the title for cross-agent references (@agent_id/title)
        let page_ref = parse_page_ref(raw_title);
        let (effective_agent_id, title) = match &page_ref {
            PageRef::SameAgent { title } => (context.agent_id.to_string(), title.as_str()),
            PageRef::CrossAgent { agent_id, title } => {
                // Cross-agent read requires scratch.cross:<target_agent_id> permission
                let cross_resource = format!("scratch.cross:{}", agent_id);
                if !context
                    .permissions
                    .check(&cross_resource, PermissionOp::Read)
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: cross_resource,
                        operation: format!("{:?}", PermissionOp::Read),
                    });
                }
                (agent_id.clone(), title.as_str())
            }
        };

        let page = match self.store.read_page(&effective_agent_id, title).await {
            Ok(page) => page,
            Err(agentos_scratch::ScratchError::PageNotFound { .. }) => {
                return Ok(serde_json::json!({
                    "found": false,
                    "title": raw_title,
                    "message": format!("No scratchpad page found with title '{}'", raw_title),
                }));
            }
            Err(e) => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "scratch-read".into(),
                    reason: format!("Read failed: {}", e),
                });
            }
        };

        let mut response = serde_json::json!({
            "found": true,
            "page_id": page.id,
            "title": page.title,
            "content": page.content,
            "tags": page.tags,
            "metadata": page.metadata,
            "created_at": page.created_at.to_rfc3339(),
            "updated_at": page.updated_at.to_rfc3339(),
        });

        // Include the agent_id in the response for cross-agent reads
        if matches!(page_ref, PageRef::CrossAgent { .. }) {
            response["agent_id"] = serde_json::Value::String(page.agent_id);
        }

        Ok(response)
    }
}
