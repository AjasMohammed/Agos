use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct ListMySchedulesTool;

impl ListMySchedulesTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ListMySchedulesTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ListMySchedulesTool {
    fn name(&self) -> &str {
        "list-my-schedules"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let kinds = payload
            .get("kinds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let include_inactive = payload
            .get("include_inactive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(serde_json::json!({
            "_kernel_action": "list_my_schedules",
            "kinds": kinds,
            "include_inactive": include_inactive,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::path::PathBuf;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: PathBuf::from("/tmp"),
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
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
        }
    }

    #[tokio::test]
    async fn emits_kernel_action_with_defaults() {
        let out = ListMySchedulesTool::new()
            .execute(serde_json::json!({}), ctx())
            .await
            .unwrap();
        assert_eq!(out["_kernel_action"], "list_my_schedules");
        assert_eq!(out["kinds"], serde_json::json!([]));
        assert_eq!(out["include_inactive"], false);
    }

    #[tokio::test]
    async fn passes_through_kinds_and_flag() {
        let out = ListMySchedulesTool::new()
            .execute(
                serde_json::json!({"kinds": ["cron", "timer"], "include_inactive": true}),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(out["kinds"], serde_json::json!(["cron", "timer"]));
        assert_eq!(out["include_inactive"], true);
    }
}
