use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp, TaskID};
use async_trait::async_trait;

pub struct TaskStatusTool;

impl TaskStatusTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TaskStatusTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for TaskStatusTool {
    fn name(&self) -> &str {
        "task-status"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("task.query".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("task.query", PermissionOp::Read) {
            return Err(AgentOSError::PermissionDenied {
                resource: "task.query".to_string(),
                operation: "Read".to_string(),
            });
        }

        let task_id_str = payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("task-status requires 'task_id' field".into())
            })?;

        let task_id: TaskID = task_id_str.parse().map_err(|_| {
            AgentOSError::SchemaValidation(format!("Invalid task_id UUID: {}", task_id_str))
        })?;

        let registry = context
            .task_registry
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: "task-status".into(),
                reason: "Task registry not available in this context".into(),
            })?;

        match registry.get_task(&task_id) {
            Some(t) => Ok(serde_json::json!({
                "found": true,
                "id": t.id.to_string(),
                "description": t.description,
                "status": t.status,
                "agent_id": t.agent_id.to_string(),
                "created_at": t.created_at.to_rfc3339(),
                "started_at": t.started_at.map(|dt| dt.to_rfc3339()),
            })),
            None => Ok(serde_json::json!({
                "found": false,
                "task_id": task_id_str,
            })),
        }
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
        permissions.grant("task.query".to_string(), true, false, false, None);
        ToolExecutionContext {
            permissions,
            ..ctx_no_perms()
        }
    }

    #[tokio::test]
    async fn task_status_requires_permission() {
        let tool = TaskStatusTool::new();
        let result = tool
            .execute(
                serde_json::json!({"task_id": "00000000-0000-0000-0000-000000000000"}),
                ctx_no_perms(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[tokio::test]
    async fn task_status_requires_task_id() {
        let tool = TaskStatusTool::new();
        let result = tool.execute(serde_json::json!({}), ctx_with_perms()).await;
        assert!(matches!(result, Err(AgentOSError::SchemaValidation(_))));
    }

    #[tokio::test]
    async fn task_status_rejects_invalid_uuid() {
        let tool = TaskStatusTool::new();
        let result = tool
            .execute(
                serde_json::json!({"task_id": "not-a-uuid"}),
                ctx_with_perms(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::SchemaValidation(_))));
    }
}
