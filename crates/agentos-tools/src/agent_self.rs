use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, AgentSelfView, PermissionEntry, PermissionOp};
use async_trait::async_trait;

/// Maximum number of tasks returned in `active_tasks`. Increasing this limit
/// has no security implications but does increase response payload size.
const MAX_TASKS: usize = 20;

/// Statuses that count as "active" — the agent is still working on the task.
const ACTIVE_STATUSES: &[&str] = &["queued", "running", "waiting"];

/// Formats a single permission entry as "resource:rwxqo", omitting letters for
/// bits that are not set (e.g. read-only → "fs.user_data:r").
fn format_permission_entry(entry: &PermissionEntry) -> String {
    let mut ops = String::with_capacity(5);
    if entry.read {
        ops.push('r');
    }
    if entry.write {
        ops.push('w');
    }
    if entry.execute {
        ops.push('x');
    }
    if entry.query {
        ops.push('q');
    }
    if entry.observe {
        ops.push('o');
    }
    format!("{}:{}", entry.resource, ops)
}

/// The `agent-self` tool.
///
/// Returns a snapshot of the calling agent's own state: current permissions,
/// budget, available tools, event subscriptions, and active tasks.
///
/// **No special permissions required** — an agent always has the right to
/// inspect its own context. Reading another agent's state requires the
/// `agent-list` / `task-list` tools with `agent.registry:r` permission.
///
/// # Constructor
/// `AgentSelfTool::new(tool_names)` — pass the list of tool names available in
/// the runner. The `ToolRunner::register_agent_self` helper populates this.
pub struct AgentSelfTool {
    tool_names: Vec<String>,
}

impl AgentSelfTool {
    pub fn new(tool_names: Vec<String>) -> Self {
        Self { tool_names }
    }
}

impl Default for AgentSelfTool {
    fn default() -> Self {
        Self::new(vec![])
    }
}

#[async_trait]
impl AgentTool for AgentSelfTool {
    fn name(&self) -> &str {
        "agent-self"
    }

    /// `agent-self` is self-scoped and read-only, so it requires no permissions.
    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        // ── Permissions ────────────────────────────────────────────────
        let permissions: Vec<String> = context
            .permissions
            .entries
            .iter()
            .map(format_permission_entry)
            .collect();

        let deny_entries = context.permissions.deny_entries.clone();

        // ── Agent name & status ────────────────────────────────────────
        let (name, status) = match &context.agent_registry {
            Some(registry) => match registry.get_agent(&context.agent_id) {
                Some(summary) => (summary.name.clone(), summary.status.clone()),
                None => (String::new(), "unknown".to_string()),
            },
            None => (String::new(), "unknown".to_string()),
        };

        // ── Active tasks ───────────────────────────────────────────────
        // Only return tasks in an active state (queued / running / waiting).
        // Completed, failed, and cancelled tasks are excluded to keep the
        // response focused on what the agent is currently doing.
        let active_tasks = match &context.task_registry {
            Some(registry) => registry
                .list_tasks_for_agent(&context.agent_id, MAX_TASKS)
                .into_iter()
                .filter(|t| ACTIVE_STATUSES.contains(&t.status.as_str()))
                .collect(),
            None => vec![],
        };

        let view = AgentSelfView {
            agent_id: context.agent_id,
            name,
            status,
            permissions,
            deny_entries,
            // Budget is not currently wired into ToolExecutionContext.
            // The field is reserved for when a CostTrackerQuery interface
            // is added to the context (see: Spec §11 cost attribution).
            budget: None,
            tools: self.tool_names.clone(),
            // Subscription query is not yet exposed via ToolExecutionContext.
            // Will be populated when an EventBusQuery interface is added.
            subscriptions: vec![],
            active_tasks,
        };

        serde_json::to_value(&view).map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "agent-self".to_string(),
            reason: format!("failed to serialise AgentSelfView: {}", e),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;
    use std::sync::Arc;

    fn make_ctx(permissions: PermissionSet) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions,
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
    async fn agent_self_returns_own_agent_id() {
        let tool = AgentSelfTool::new(vec![]);
        let agent_id = AgentID::new();
        let ctx = ToolExecutionContext {
            agent_id,
            ..make_ctx(PermissionSet::new())
        };

        let result = tool.execute(serde_json::json!({}), ctx).await.unwrap();
        assert_eq!(
            result["agent_id"].as_str().unwrap(),
            agent_id.to_string().as_str()
        );
    }

    #[tokio::test]
    async fn agent_self_works_without_any_permissions() {
        let tool = AgentSelfTool::new(vec![]);
        let result = tool
            .execute(serde_json::json!({}), make_ctx(PermissionSet::new()))
            .await;
        assert!(
            result.is_ok(),
            "agent-self must succeed with an empty permission set"
        );
    }

    #[tokio::test]
    async fn agent_self_formats_permission_entries() {
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        perms.grant("memory.semantic".to_string(), true, true, false, None);

        let tool = AgentSelfTool::new(vec![]);
        let result = tool
            .execute(serde_json::json!({}), make_ctx(perms))
            .await
            .unwrap();

        let permissions: Vec<String> = serde_json::from_value(result["permissions"].clone())
            .expect("permissions must be a string array");
        assert!(
            permissions.contains(&"fs.user_data:r".to_string()),
            "expected fs.user_data:r, got {:?}",
            permissions
        );
        assert!(
            permissions.contains(&"memory.semantic:rw".to_string()),
            "expected memory.semantic:rw, got {:?}",
            permissions
        );
    }

    #[tokio::test]
    async fn agent_self_includes_deny_entries() {
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        perms.deny_entries.push("fs:/etc/".to_string());

        let tool = AgentSelfTool::new(vec![]);
        let result = tool
            .execute(serde_json::json!({}), make_ctx(perms))
            .await
            .unwrap();

        let deny: Vec<String> = serde_json::from_value(result["deny_entries"].clone()).unwrap();
        assert!(deny.contains(&"fs:/etc/".to_string()));
    }

    #[tokio::test]
    async fn agent_self_returns_tool_names() {
        let tool_names = vec!["file-reader".to_string(), "shell-exec".to_string()];
        let tool = AgentSelfTool::new(tool_names.clone());
        let result = tool
            .execute(serde_json::json!({}), make_ctx(PermissionSet::new()))
            .await
            .unwrap();

        let returned: Vec<String> = serde_json::from_value(result["tools"].clone()).unwrap();
        assert_eq!(returned, tool_names);
    }

    #[tokio::test]
    async fn agent_self_without_registry_returns_unknown_status() {
        let tool = AgentSelfTool::new(vec![]);
        let result = tool
            .execute(serde_json::json!({}), make_ctx(PermissionSet::new()))
            .await
            .unwrap();

        assert_eq!(result["status"].as_str().unwrap(), "unknown");
        assert_eq!(result["name"].as_str().unwrap(), "");
    }

    #[tokio::test]
    async fn agent_self_returns_agent_name_from_registry() {
        let agent_id = AgentID::new();
        let mut perms = PermissionSet::new();
        perms.grant("agent.registry".to_string(), true, false, false, None);

        let registry = Arc::new(AgentRegistrySnapshot::new(vec![AgentSummary {
            id: agent_id,
            name: "my-agent".to_string(),
            status: "idle".to_string(),
            registered_at: chrono::Utc::now(),
        }]));

        let ctx = ToolExecutionContext {
            agent_id,
            agent_registry: Some(registry),
            ..make_ctx(perms)
        };

        let tool = AgentSelfTool::new(vec![]);
        let result = tool.execute(serde_json::json!({}), ctx).await.unwrap();

        assert_eq!(result["name"].as_str().unwrap(), "my-agent");
        assert_eq!(result["status"].as_str().unwrap(), "idle");
    }

    #[tokio::test]
    async fn agent_self_registry_present_but_agent_not_registered_returns_unknown() {
        // Edge case: registry exists but this agent's ID is not in it
        // (e.g. a freshly spawned agent that has not registered yet).
        let agent_id = AgentID::new();
        let registry = Arc::new(AgentRegistrySnapshot::new(vec![
            // Different agent in the registry — must not affect the result.
            AgentSummary {
                id: AgentID::new(),
                name: "other-agent".to_string(),
                status: "online".to_string(),
                registered_at: chrono::Utc::now(),
            },
        ]));

        let ctx = ToolExecutionContext {
            agent_id,
            agent_registry: Some(registry),
            ..make_ctx(PermissionSet::new())
        };

        let tool = AgentSelfTool::new(vec![]);
        let result = tool.execute(serde_json::json!({}), ctx).await.unwrap();

        assert_eq!(result["status"].as_str().unwrap(), "unknown");
        assert_eq!(result["name"].as_str().unwrap(), "");
        // agent_id must still be the caller's ID, not any other agent's
        assert_eq!(
            result["agent_id"].as_str().unwrap(),
            agent_id.to_string().as_str()
        );
    }

    #[tokio::test]
    async fn agent_self_lists_active_tasks_for_own_agent() {
        let agent_id = AgentID::new();
        let task_id = TaskID::new();

        let tasks = vec![
            TaskIntrospectionSummary {
                id: task_id,
                agent_id,
                description: "Analyse the logs".to_string(),
                status: "running".to_string(),
                created_at: chrono::Utc::now(),
                started_at: Some(chrono::Utc::now()),
            },
            // Completed task for the same agent — must NOT appear in active_tasks.
            TaskIntrospectionSummary {
                id: TaskID::new(),
                agent_id,
                description: "Already done".to_string(),
                status: "complete".to_string(),
                created_at: chrono::Utc::now(),
                started_at: Some(chrono::Utc::now()),
            },
            // Task belonging to a different agent — must NOT appear.
            TaskIntrospectionSummary {
                id: TaskID::new(),
                agent_id: AgentID::new(),
                description: "Other agent task".to_string(),
                status: "running".to_string(),
                created_at: chrono::Utc::now(),
                started_at: None,
            },
        ];

        let task_registry = Arc::new(TaskSnapshot::new(tasks));
        let ctx = ToolExecutionContext {
            agent_id,
            task_registry: Some(task_registry),
            ..make_ctx(PermissionSet::new())
        };

        let tool = AgentSelfTool::new(vec![]);
        let result = tool.execute(serde_json::json!({}), ctx).await.unwrap();

        let active = result["active_tasks"].as_array().unwrap();
        assert_eq!(
            active.len(),
            1,
            "only the calling agent's tasks should appear"
        );
        assert_eq!(
            active[0]["id"].as_str().unwrap(),
            task_id.to_string().as_str()
        );
    }

    #[tokio::test]
    async fn agent_self_budget_is_none_by_default() {
        let tool = AgentSelfTool::new(vec![]);
        let result = tool
            .execute(serde_json::json!({}), make_ctx(PermissionSet::new()))
            .await
            .unwrap();
        assert!(result["budget"].is_null(), "budget should be None/null");
    }

    #[tokio::test]
    async fn agent_self_subscriptions_empty_by_default() {
        let tool = AgentSelfTool::new(vec![]);
        let result = tool
            .execute(serde_json::json!({}), make_ctx(PermissionSet::new()))
            .await
            .unwrap();
        assert!(
            result["subscriptions"].as_array().unwrap().is_empty(),
            "subscriptions should be empty when no event bus is wired"
        );
    }
}
