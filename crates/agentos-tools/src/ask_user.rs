use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Ask the user a blocking question and wait for a response.
///
/// The task pauses in `Waiting` state until the user submits a response via the
/// CLI (`agentctl notifications respond`) or the web UI.  An optional timeout
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

        let priority = payload
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("info")
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
