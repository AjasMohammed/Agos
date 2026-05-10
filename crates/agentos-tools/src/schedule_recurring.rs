use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::schedule::BLOCKED_SCHEDULE_TOOL_NAMES;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct ScheduleRecurringTool;

impl ScheduleRecurringTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ScheduleRecurringTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ScheduleRecurringTool {
    fn name(&self) -> &str {
        "schedule-recurring"
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
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("schedule-recurring requires 'name'".into())
            })?
            .to_string();
        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "schedule-recurring 'name' must be 1-128 characters".into(),
            ));
        }

        // Accept either `cron` or `cron_expression` for forward-compat with
        // the operator CLI flag and external docs that use the longer form.
        let cron = payload
            .get("cron")
            .or_else(|| payload.get("cron_expression"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "schedule-recurring requires 'cron' (or 'cron_expression')".into(),
                )
            })?
            .to_string();

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
                            "schedule-recurring mode=notify requires 'notify_subject'".into(),
                        )
                    })?
                    .to_string();
                let body = payload
                    .get("notify_body")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "schedule-recurring mode=notify requires 'notify_body'".into(),
                        )
                    })?
                    .to_string();
                let priority = payload
                    .get("notify_priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_string();
                Ok(serde_json::json!({
                    "_kernel_action": "create_schedule",
                    "name": name,
                    "cron": cron,
                    "agent_name": agent_name,
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
                            "schedule-recurring mode=tool requires 'tool'".into(),
                        )
                    })?
                    .to_string();
                if BLOCKED_SCHEDULE_TOOL_NAMES.contains(&tool_name.as_str()) {
                    return Err(AgentOSError::SchemaValidation(format!(
                        "schedule-recurring: tool '{}' cannot be scheduled (anti-recursion guard)",
                        tool_name
                    )));
                }
                let tool_args = payload
                    .get("tool_args")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                Ok(serde_json::json!({
                    "_kernel_action": "create_schedule",
                    "name": name,
                    "cron": cron,
                    "agent_name": agent_name,
                    "mode": "tool",
                    "tool": tool_name,
                    "tool_args": tool_args,
                }))
            }
            "task" | "" => {
                let task_prompt = payload
                    .get("task_prompt")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "schedule-recurring mode=task requires 'task_prompt'".into(),
                        )
                    })?
                    .to_string();
                Ok(serde_json::json!({
                    "_kernel_action": "create_schedule",
                    "name": name,
                    "cron": cron,
                    "agent_name": agent_name,
                    "mode": "task",
                    "task_prompt": task_prompt,
                }))
            }
            other => Err(AgentOSError::SchemaValidation(format!(
                "schedule-recurring 'mode' must be one of [task, notify, tool] (got '{}')",
                other
            ))),
        }
    }
}
