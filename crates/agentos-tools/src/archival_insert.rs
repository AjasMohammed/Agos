use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::SemanticStore;
use agentos_types::*;
use async_trait::async_trait;
use std::sync::Arc;

pub struct ArchivalInsert {
    semantic: Arc<SemanticStore>,
}

impl ArchivalInsert {
    pub fn new(semantic: Arc<SemanticStore>) -> Self {
        Self { semantic }
    }
}

#[async_trait]
impl AgentTool for ArchivalInsert {
    fn name(&self) -> &str {
        "archival-insert"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Write)]
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
                AgentOSError::SchemaValidation("archival-insert requires 'content'".into())
            })?;
        let key = payload
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("archival-note");
        let tags: Vec<&str> = payload
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let id = self
            .semantic
            .write(key, content, Some(&context.agent_id), &tags)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "archival-insert".to_string(),
                reason: e.to_string(),
            })?;
        Ok(serde_json::json!({ "success": true, "id": id }))
    }
}
