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
