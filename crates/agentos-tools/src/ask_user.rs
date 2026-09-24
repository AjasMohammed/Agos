use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Ask the user a blocking question and wait for a response.
///
/// The task pauses in `Waiting` state until the user submits a response via the
/// CLI (`agentos notifications respond`) or the web UI.  An optional timeout
/// controls how long the kernel waits before auto-responding with the
/// `auto_action` text (default: "auto_denied").
///
/// Requires `user.interact:x` permission.
pub struct AskUserTool;

impl AskUserTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AskUserTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AskUserTool {
    fn name(&self) -> &str {
        "ask-user"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("user.interact".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let question = payload
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("ask-user requires 'question' field".into())
            })?
            .to_string();

        let options: Option<Vec<String>> =
            payload
                .get("options")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                });

        let timeout_secs = payload
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(300);

        // Models reach for low/medium/high — the near-universal severity words —
        // and used to lose a whole call to a schema rejection. Accept them and
        // fold them onto the canonical levels so downstream stays unchanged.
        let priority = match payload
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("info")
        {
            "low" => "info",
            "medium" => "warning",
            "high" => "urgent",
            other => other,
        }
        .to_string();

        let auto_action = payload
            .get("auto_action")
            .and_then(|v| v.as_str())
            .unwrap_or("auto_denied")
            .to_string();

        let mut result = serde_json::json!({
            "_kernel_action": "ask_user",
            "question": question,
            "timeout_secs": timeout_secs,
            "priority": priority,
            "auto_action": auto_action,
        });

        if let Some(opts) = options {
            result["options"] =
                serde_json::Value::Array(opts.into_iter().map(serde_json::Value::String).collect());
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;
    use serde_json::json;

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
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
            shared_dir: None,
        }
    }

    async fn priority_of(payload: serde_json::Value) -> String {
        let out = AskUserTool::new()
            .execute(payload, ctx())
            .await
            .expect("ask-user builds a kernel action");
        out["priority"].as_str().expect("priority").to_string()
    }

    /// A model reaching for "high" used to lose a whole call to a schema
    /// rejection (2026-09-08). Aliases fold onto the canonical levels so
    /// nothing downstream has to learn the new words.
    #[tokio::test]
    async fn severity_aliases_fold_onto_canonical_levels() {
        assert_eq!(
            priority_of(json!({"question": "q", "priority": "high"})).await,
            "urgent"
        );
        assert_eq!(
            priority_of(json!({"question": "q", "priority": "medium"})).await,
            "warning"
        );
        assert_eq!(
            priority_of(json!({"question": "q", "priority": "low"})).await,
            "info"
        );
    }

    #[tokio::test]
    async fn canonical_levels_and_the_default_are_untouched() {
        assert_eq!(
            priority_of(json!({"question": "q", "priority": "critical"})).await,
            "critical"
        );
        assert_eq!(priority_of(json!({"question": "q"})).await, "info");
    }
}
