use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_scratch::ScratchpadStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ScratchWriteTool {
    store: Arc<ScratchpadStore>,
}

impl ScratchWriteTool {
    pub fn new(store: Arc<ScratchpadStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for ScratchWriteTool {
    fn name(&self) -> &str {
        "scratch-write"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("scratchpad".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("scratchpad", PermissionOp::Write) {
            return Err(AgentOSError::PermissionDenied {
                resource: "scratchpad".to_string(),
                operation: format!("{:?}", PermissionOp::Write),
            });
        }

        let title = payload
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "scratch-write requires 'title' field (string)".into(),
                )
            })?;

        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "scratch-write requires 'content' field (string)".into(),
                )
            })?;

        let tags: Vec<String> = match payload.get("tags") {
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        };

        let agent_id = context.agent_id.to_string();
        let page = self
            .store
            .write_page(&agent_id, title, content, &tags)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "scratch-write".into(),
                reason: format!("Write failed: {}", e),
            })?;

        Ok(serde_json::json!({
            "success": true,
            "page_id": page.id,
            "title": page.title,
            "tags": page.tags,
            "created_at": page.created_at.to_rfc3339(),
            "updated_at": page.updated_at.to_rfc3339(),
        }))
    }
}
