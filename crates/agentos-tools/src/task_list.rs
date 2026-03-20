use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct TaskListTool;

impl TaskListTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TaskListTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for TaskListTool {
    fn name(&self) -> &str {
        "task-list"
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

        let registry = context
            .task_registry
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: "task-list".into(),
                reason: "Task registry not available in this context".into(),
            })?;

        let filter = payload
            .get("filter")
            .and_then(|v| v.as_str())
            .unwrap_or("mine");

        if filter != "mine" && filter != "active" {
            return Err(AgentOSError::SchemaValidation(format!(
                "Invalid filter '{}'. Valid values: mine, active",
                filter
            )));
        }

        let limit = (payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize).min(100);

        let tasks = match filter {
            "active" => registry.list_active_tasks(limit),
            _ => registry.list_tasks_for_agent(&context.agent_id, limit),
        };

        let serialized: Vec<_> = tasks
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id.to_string(),
                    "description": t.description,
                    "status": t.status,
                    "agent_id": t.agent_id.to_string(),
                    "created_at": t.created_at.to_rfc3339(),
                    "started_at": t.started_at.map(|dt| dt.to_rfc3339()),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "filter": filter,
            "count": serialized.len(),
            "tasks": serialized,
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
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn task_list_requires_permission() {
        let tool = TaskListTool::new();
        let result = tool.execute(serde_json::json!({}), ctx_no_perms()).await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }
}
