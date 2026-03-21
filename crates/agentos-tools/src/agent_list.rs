use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct AgentListTool;

impl AgentListTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentListTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AgentListTool {
    fn name(&self) -> &str {
        "agent-list"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.registry".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("agent.registry", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "agent.registry".to_string(),
                operation: "Read".to_string(),
            });
        }

        let registry = context
            .agent_registry
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: "agent-list".into(),
                reason: "Agent registry not available in this execution context".into(),
            })?;

        let status_filter = payload
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());

        let agents = registry.list_agents();
        let filtered: Vec<_> = agents
            .into_iter()
            .filter(|a| {
                status_filter
                    .as_ref()
                    .map(|f| a.status == *f)
                    .unwrap_or(true)
            })
            .map(|a| {
                serde_json::json!({
                    "id": a.id.to_string(),
                    "name": a.name,
                    "status": a.status,
                    "registered_at": a.registered_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "count": filtered.len(),
            "agents": filtered,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;

    fn ctx_no_perms() -> ToolExecutionContext {
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
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn ctx_with_perms() -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant("agent.registry".to_string(), true, false, false, None);
        ToolExecutionContext {
            permissions,
            ..ctx_no_perms()
        }
    }

    #[tokio::test]
    async fn agent_list_requires_permission() {
        let tool = AgentListTool::new();
        let result = tool.execute(serde_json::json!({}), ctx_no_perms()).await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[tokio::test]
    async fn agent_list_returns_error_without_registry() {
        let tool = AgentListTool::new();
        let result = tool.execute(serde_json::json!({}), ctx_with_perms()).await;
        assert!(matches!(
            result,
            Err(AgentOSError::ToolExecutionFailed { .. })
        ));
    }
}
