use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{schedule::TimerAction, AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Create a one-shot timer that fires after a delay.
///
/// When the timer fires, the kernel can:
/// - Send a notification to the user (`action: "notify"`)
/// - Run a task prompt on an agent (`action: "run_task"`)
/// - Both (`action: "run_task_and_notify"`)
///
/// Requires `schedule.timer:w` permission.
pub struct SetTimerTool;

impl SetTimerTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SetTimerTool {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_priority(payload: &serde_json::Value) -> Result<String, AgentOSError> {
    let priority = payload
        .get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or("info");
    match priority {
        p @ ("info" | "warning" | "urgent" | "critical") => Ok(p.to_string()),
        other => Err(AgentOSError::SchemaValidation(format!(
            "Invalid priority '{}'. Valid values: info, warning, urgent, critical",
            other
        ))),
    }
}

#[async_trait]
impl AgentTool for SetTimerTool {
    fn name(&self) -> &str {
        "set-timer"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("schedule.timer".to_string(), PermissionOp::Write)]
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
                AgentOSError::SchemaValidation("set-timer requires 'name' field".into())
            })?
            .to_string();

        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "set-timer 'name' must be 1-128 characters".into(),
            ));
        }

        let delay_secs = payload
            .get("delay_secs")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "set-timer requires 'delay_secs' field (positive integer)".into(),
                )
            })?;

        if delay_secs == 0 || delay_secs > 86400 {
            return Err(AgentOSError::SchemaValidation(
                "set-timer 'delay_secs' must be between 1 and 86400 (24 hours)".into(),
            ));
        }

        let action_type = payload
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("notify");

        // The agent_name for the timer target. If not provided in the payload,
        // fall back to the calling agent's ID string so the kernel can resolve it.
        let agent_name = payload
            .get("agent_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| context.agent_id.to_string());

        // Construct TimerAction as a typed value and serialize once.
        // This ensures serde format stays consistent with TimerAction's derive.
        let timer_action: TimerAction = match action_type {
            "notify" => {
                let subject = payload
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Timer fired")
                    .to_string();
                let body = payload
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let priority = parse_priority(&payload)?;
                TimerAction::NotifyUser {
                    subject,
                    body,
                    priority,
                }
            }
            "run_task" => {
                let prompt = payload
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "action 'run_task' requires 'prompt' field".into(),
                        )
                    })?
                    .to_string();
                TimerAction::RunTask { prompt }
            }
            "run_task_and_notify" => {
                let prompt = payload
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "action 'run_task_and_notify' requires 'prompt' field".into(),
                        )
                    })?
                    .to_string();
                let subject = payload
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Timer fired")
                    .to_string();
                let body = payload
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let priority = parse_priority(&payload)?;
                TimerAction::RunTaskAndNotify {
                    prompt,
                    subject,
                    body,
                    priority,
                }
            }
            other => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Unknown timer action '{}'. Valid: notify, run_task, run_task_and_notify",
                    other
                )));
            }
        };

        let action_value = serde_json::to_value(&timer_action).map_err(|e| {
            AgentOSError::SchemaValidation(format!("Failed to serialize timer action: {}", e))
        })?;

        Ok(serde_json::json!({
            "_kernel_action": "set_timer",
            "name": name,
            "delay_secs": delay_secs,
            "agent_name": agent_name,
            "action": action_value,
        }))
    }
}

/// Cancel a pending timer by name.
///
/// Requires `schedule.timer:w` permission.
pub struct CancelTimerTool;

impl CancelTimerTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CancelTimerTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for CancelTimerTool {
    fn name(&self) -> &str {
        "cancel-timer"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("schedule.timer".to_string(), PermissionOp::Write)]
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
                AgentOSError::SchemaValidation("cancel-timer requires 'name' field".into())
            })?
            .to_string();

        if name.is_empty() || name.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "cancel-timer 'name' must be 1-128 characters".into(),
            ));
        }

        Ok(serde_json::json!({
            "_kernel_action": "cancel_timer",
            "name": name,
        }))
    }
}

/// List all pending timers.
///
/// Requires `schedule.timer:r` permission.
pub struct ListTimersTool;

impl ListTimersTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ListTimersTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ListTimersTool {
    fn name(&self) -> &str {
        "list-timers"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("schedule.timer".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "_kernel_action": "list_timers",
        }))
    }
}
