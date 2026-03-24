use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use chrono::Utc;

pub struct DatetimeTool;

impl DatetimeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DatetimeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for DatetimeTool {
    fn name(&self) -> &str {
        "datetime"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![] // no permissions required
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let now = Utc::now();
        Ok(serde_json::json!({
            "utc_iso8601": now.to_rfc3339(),
            "unix_timestamp_secs": now.timestamp(),
            "unix_timestamp_millis": now.timestamp_millis(),
            "date": now.format("%Y-%m-%d").to_string(),
            "time": now.format("%H:%M:%S").to_string(),
            "timezone": "UTC",
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
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn datetime_returns_utc_fields() {
        let tool = DatetimeTool::new();
        let result = tool.execute(serde_json::json!({}), ctx()).await.unwrap();
        assert!(result["utc_iso8601"].as_str().unwrap().contains('T'));
        assert!(result["unix_timestamp_secs"].as_i64().unwrap() > 0);
        assert!(result["unix_timestamp_millis"].as_i64().unwrap() > 0);
        assert_eq!(result["timezone"], "UTC");
        // date should be YYYY-MM-DD format (10 chars)
        assert_eq!(result["date"].as_str().unwrap().len(), 10);
        // time should be HH:MM:SS format (8 chars)
        assert_eq!(result["time"].as_str().unwrap().len(), 8);
    }
}
