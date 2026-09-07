use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use chrono::{Local, Utc};

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
        let utc_now = Utc::now();
        let local_now = Local::now();
        Ok(serde_json::json!({
            "utc_iso8601": utc_now.to_rfc3339(),
            "local_iso8601": local_now.to_rfc3339(),
            "unix_timestamp_secs": utc_now.timestamp(),
            "unix_timestamp_millis": utc_now.timestamp_millis(),
            "utc_date": utc_now.format("%Y-%m-%d").to_string(),
            "utc_time": utc_now.format("%H:%M:%S").to_string(),
            "local_date": local_now.format("%Y-%m-%d").to_string(),
            "local_time": local_now.format("%H:%M:%S").to_string(),
            "utc_offset": local_now.format("%:z").to_string(),
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
    async fn datetime_returns_utc_and_local_fields() {
        let tool = DatetimeTool::new();
        let result = tool.execute(serde_json::json!({}), ctx()).await.unwrap();
        assert!(result["utc_iso8601"].as_str().unwrap().contains('T'));
        assert!(result["local_iso8601"].as_str().unwrap().contains('T'));
        assert!(result["unix_timestamp_secs"].as_i64().unwrap() > 0);
        assert!(result["unix_timestamp_millis"].as_i64().unwrap() > 0);
        assert_eq!(result["utc_date"].as_str().unwrap().len(), 10);
        assert_eq!(result["utc_time"].as_str().unwrap().len(), 8);
        assert_eq!(result["local_date"].as_str().unwrap().len(), 10);
        assert_eq!(result["local_time"].as_str().unwrap().len(), 8);
        // offset format: +HH:MM or -HH:MM
        let offset = result["utc_offset"].as_str().unwrap();
        assert!(offset.starts_with('+') || offset.starts_with('-'));
    }
}
