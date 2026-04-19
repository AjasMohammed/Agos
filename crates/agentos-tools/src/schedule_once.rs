use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use chrono::{Duration, Utc};

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
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation("schedule-once requires 'name'".into()))?
            .to_string();

        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "schedule-once 'name' must be 1-128 characters".into(),
            ));
        }

        let task_prompt = payload
            .get("task_prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("schedule-once requires 'task_prompt'".into())
            })?
            .to_string();

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

        Ok(serde_json::json!({
            "_kernel_action": "schedule_once",
            "name": name,
            "task_prompt": task_prompt,
            "agent_name": agent_name,
            "fire_at": fire_at.to_rfc3339(),
        }))
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
