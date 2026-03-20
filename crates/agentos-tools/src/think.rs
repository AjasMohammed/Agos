use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct ThinkTool;

impl ThinkTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ThinkTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ThinkTool {
    fn name(&self) -> &str {
        "think"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![] // no permissions required
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let thought = payload
            .get("thought")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("think requires 'thought' field".into())
            })?;

        // The tool is a deliberate no-op: the ToolRunner already records every
        // tool call + result in the audit log, so the thought is captured at
        // the call boundary without any additional write here.
        Ok(serde_json::json!({
            "acknowledged": true,
            "thought_length": thought.len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn think_returns_acknowledged() {
        let tool = ThinkTool::new();
        let result = tool
            .execute(serde_json::json!({"thought": "test reasoning"}), ctx())
            .await
            .unwrap();
        assert_eq!(result["acknowledged"], true);
        assert_eq!(result["thought_length"], 14);
    }

    #[tokio::test]
    async fn think_requires_thought_field() {
        let tool = ThinkTool::new();
        let result = tool.execute(serde_json::json!({}), ctx()).await;
        assert!(matches!(result, Err(AgentOSError::SchemaValidation(_))));
    }
}
