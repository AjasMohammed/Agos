use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::schedule::BLOCKED_SCHEDULE_TOOL_NAMES;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use uuid::Uuid;

/// Schedule a one-shot task at an absolute datetime or relative delay.
/// Use `fire_at` (ISO 8601 / RFC 3339) for an exact moment, or `delay_secs`
/// for a relative delay. The kernel fires it once and discards the entry.
pub struct ScheduleOnceTool;

impl ScheduleOnceTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ScheduleOnceTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ScheduleOnceTool {
    fn name(&self) -> &str {
        "schedule-once"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("schedule.job".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        // `name` is optional. When omitted (common with small models that drop
        // optional-looking fields), auto-generate `auto-once-<8hex>` so the job
        // is still identifiable and cancellable.
        let mode_for_name = payload
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("task");
        let name = match payload.get("name").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                let suffix = Uuid::new_v4().simple().to_string();
                format!("auto-{}-{}", mode_for_name, &suffix[..8])
            }
        };

        if name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "schedule-once 'name' must be 1-128 characters".into(),
            ));
        }

        // Resolve fire_at: either an explicit ISO 8601 datetime or delay_secs.
        let fire_at: chrono::DateTime<chrono::Utc> =
            if let Some(ts) = payload.get("fire_at").and_then(|v| v.as_str()) {
                ts.parse::<chrono::DateTime<chrono::Utc>>().map_err(|_| {
                    AgentOSError::SchemaValidation(format!(
                        "schedule-once 'fire_at' must be ISO 8601 / RFC 3339 (got '{}')",
                        ts
                    ))
                })?
            } else if let Some(secs) = payload.get("delay_secs").and_then(|v| v.as_u64()) {
                if secs == 0 || secs > 86400 {
                    return Err(AgentOSError::SchemaValidation(
                        "schedule-once 'delay_secs' must be 1-86400".into(),
                    ));
                }
                Utc::now() + Duration::seconds(secs as i64)
            } else {
                return Err(AgentOSError::SchemaValidation(
                    "schedule-once requires either 'fire_at' (ISO 8601) or 'delay_secs'".into(),
                ));
            };

        let now = Utc::now();
        if fire_at <= now {
            return Err(AgentOSError::SchemaValidation(
                "schedule-once 'fire_at' must be in the future".into(),
            ));
        }
        let max_horizon = now + Duration::days(30);
        if fire_at > max_horizon {
            return Err(AgentOSError::SchemaValidation(
                "schedule-once 'fire_at' must be within 30 days of now".into(),
            ));
        }

        let agent_name = payload
            .get("agent_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| context.agent_id.to_string());

        let mode = payload
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("task");

        match mode {
            "notify" => {
                let subject = payload
                    .get("notify_subject")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "schedule-once mode=notify requires 'notify_subject'".into(),
                        )
                    })?
                    .to_string();
                let body = payload
                    .get("notify_body")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "schedule-once mode=notify requires 'notify_body'".into(),
                        )
                    })?
                    .to_string();
                let priority = payload
                    .get("notify_priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_string();
                Ok(serde_json::json!({
                    "_kernel_action": "schedule_once",
                    "name": name,
                    "agent_name": agent_name,
                    "fire_at": fire_at.to_rfc3339(),
                    "mode": "notify",
                    "notify_subject": subject,
                    "notify_body": body,
                    "notify_priority": priority,
                }))
            }
            "tool" => {
                let tool_name = payload
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "schedule-once mode=tool requires 'tool'".into(),
                        )
                    })?
                    .to_string();
                if BLOCKED_SCHEDULE_TOOL_NAMES.contains(&tool_name.as_str()) {
                    return Err(AgentOSError::SchemaValidation(format!(
                        "schedule-once: tool '{}' cannot be scheduled",
                        tool_name
                    )));
                }
                let tool_args = payload
                    .get("tool_args")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                Ok(serde_json::json!({
                    "_kernel_action": "schedule_once",
                    "name": name,
                    "agent_name": agent_name,
                    "fire_at": fire_at.to_rfc3339(),
                    "mode": "tool",
                    "tool": tool_name,
                    "tool_args": tool_args,
                }))
            }
            _ => {
                // Default: "task" mode
                let task_prompt = payload
                    .get("task_prompt")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "schedule-once mode=task requires 'task_prompt'".into(),
                        )
                    })?
                    .to_string();
                Ok(serde_json::json!({
                    "_kernel_action": "schedule_once",
                    "name": name,
                    "agent_name": agent_name,
                    "fire_at": fire_at.to_rfc3339(),
                    "mode": "task",
                    "task_prompt": task_prompt,
                }))
            }
        }
    }
}

/// Cancel a pending once-job by name.
pub struct CancelOnceJobTool;

impl CancelOnceJobTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CancelOnceJobTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for CancelOnceJobTool {
    fn name(&self) -> &str {
        "cancel-once-job"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("schedule.job".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("cancel-once-job requires 'name'".into())
            })?
            .to_string();

        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "cancel-once-job 'name' must be 1-128 characters".into(),
            ));
        }

        Ok(serde_json::json!({
            "_kernel_action": "cancel_once_job",
            "name": name,
        }))
    }
}

/// List all pending once-jobs.
pub struct ListOnceJobsTool;

impl ListOnceJobsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ListOnceJobsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ListOnceJobsTool {
    fn name(&self) -> &str {
        "list-once-jobs"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("schedule.job".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "_kernel_action": "list_once_jobs",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::path::PathBuf;

    fn ctx() -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant("schedule.job".to_string(), true, true, false, None);
        ToolExecutionContext {
            data_dir: PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions,
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
    async fn test_schedule_once_auto_generates_name_when_omitted() {
        let tool = ScheduleOnceTool::new();
        let payload = serde_json::json!({
            "delay_secs": 60,
            "task_prompt": "ping",
        });
        let result = tool.execute(payload, ctx()).await.unwrap();
        let name = result["name"].as_str().unwrap();
        assert!(name.starts_with("auto-task-"), "name was {name}");
        assert_eq!(name.len(), "auto-task-".len() + 8);
    }

    #[tokio::test]
    async fn test_schedule_once_auto_name_uses_mode() {
        let tool = ScheduleOnceTool::new();
        let payload = serde_json::json!({
            "delay_secs": 60,
            "mode": "notify",
            "notify_subject": "s",
            "notify_body": "b",
        });
        let result = tool.execute(payload, ctx()).await.unwrap();
        let name = result["name"].as_str().unwrap();
        assert!(name.starts_with("auto-notify-"), "name was {name}");
    }

    #[tokio::test]
    async fn test_schedule_once_explicit_name_preserved() {
        let tool = ScheduleOnceTool::new();
        let payload = serde_json::json!({
            "name": "my-job",
            "delay_secs": 60,
            "task_prompt": "ping",
        });
        let result = tool.execute(payload, ctx()).await.unwrap();
        assert_eq!(result["name"], "my-job");
    }

    #[tokio::test]
    async fn test_schedule_once_empty_name_falls_back_to_auto() {
        let tool = ScheduleOnceTool::new();
        let payload = serde_json::json!({
            "name": "",
            "delay_secs": 60,
            "task_prompt": "ping",
        });
        let result = tool.execute(payload, ctx()).await.unwrap();
        let name = result["name"].as_str().unwrap();
        assert!(name.starts_with("auto-task-"));
    }
}
