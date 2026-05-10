use crate::*;
use serde::{Deserialize, Serialize};

/// What the kernel does when a timer fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimerAction {
    /// Send a notification to the user inbox.
    NotifyUser {
        subject: String,
        body: String,
        /// "info" | "warning" | "urgent" | "critical"
        priority: String,
    },
    /// Run a task prompt on the timer's agent.
    RunTask { prompt: String },
    /// Run a task AND send a user notification.
    RunTaskAndNotify {
        prompt: String,
        subject: String,
        body: String,
        priority: String,
    },
    /// Invoke a single tool with a fixed JSON arg payload. No LLM in the loop.
    RunTool {
        tool: String,
        args: serde_json::Value,
    },
}

/// What the kernel does when a once-job fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OnceJobAction {
    /// Run a task prompt on the job's agent. Bounded execution.
    RunTask { prompt: String },
    /// Deliver a notification directly via the notification router. No LLM.
    NotifyUser {
        subject: String,
        body: String,
        /// "info" | "warning" | "urgent" | "critical"
        priority: String,
    },
    /// Invoke a single tool with a fixed JSON arg payload. No LLM in the loop.
    RunTool {
        tool: String,
        args: serde_json::Value,
    },
}

impl OnceJobAction {
    pub fn run_task(prompt: impl Into<String>) -> Self {
        Self::RunTask {
            prompt: prompt.into(),
        }
    }

    pub fn notify_user(
        subject: impl Into<String>,
        body: impl Into<String>,
        priority: impl Into<String>,
    ) -> Self {
        Self::NotifyUser {
            subject: subject.into(),
            body: body.into(),
            priority: priority.into(),
        }
    }

    pub fn run_tool(tool: impl Into<String>, args: serde_json::Value) -> Self {
        Self::RunTool {
            tool: tool.into(),
            args,
        }
    }

    /// Short tag used in audit logs / telemetry.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::RunTask { .. } => "run_task",
            Self::NotifyUser { .. } => "notify_user",
            Self::RunTool { .. } => "run_tool",
        }
    }
}

/// A pending one-shot timer. Removed from the store once it fires or is cancelled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerEntry {
    pub id: ScheduleID,
    pub name: String,
    pub agent_name: String,
    pub fire_at: chrono::DateTime<chrono::Utc>,
    pub action: TimerAction,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Agent that created this timer. Enforced on cancel from agent tools.
    /// `#[serde(default)]` keeps the field back-compat with pre-existing rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_agent_id: Option<crate::AgentID>,
    /// How the fired result is delivered. Defaults to `Silent` for legacy rows.
    #[serde(default)]
    pub delivery: crate::delivery::DeliveryMode,
}

/// Lifecycle state of a one-shot scheduled job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnceJobState {
    Pending,
    Fired,
    Cancelled,
}

/// A one-shot scheduled job that runs at a specific datetime.
///
/// `action` is the source of truth for what fires. `task_prompt` is kept as a
/// shadow field for storage back-compat (the SQL column is `NOT NULL`); for
/// non-`RunTask` actions it carries a `[<action_tag>]` placeholder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnceJob {
    pub id: ScheduleID,
    pub name: String,
    pub agent_name: String,
    /// Legacy prompt field. Mirrors `RunTask.prompt` when `action` is `RunTask`,
    /// otherwise carries a placeholder string for the legacy SQL column.
    pub task_prompt: String,
    pub fire_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub state: OnceJobState,
    /// What fires when this job triggers. Source of truth.
    #[serde(default = "default_run_task_action")]
    pub action: OnceJobAction,
    /// Agent that created this once-job. Enforced on cancel from agent tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_agent_id: Option<crate::AgentID>,
    /// How the fired result is delivered. Defaults to `Silent` for legacy rows.
    #[serde(default)]
    pub delivery: crate::delivery::DeliveryMode,
}

fn default_run_task_action() -> OnceJobAction {
    OnceJobAction::RunTask {
        prompt: String::new(),
    }
}

impl OnceJob {
    /// Synthesize the legacy `task_prompt` shadow string for an action.
    pub fn shadow_task_prompt(action: &OnceJobAction) -> String {
        match action {
            OnceJobAction::RunTask { prompt } => prompt.clone(),
            OnceJobAction::NotifyUser { .. } => "[notify-user]".to_string(),
            OnceJobAction::RunTool { tool, .. } => format!("[run-tool:{}]", tool),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: ScheduleID,
    pub name: String,
    pub cron_expression: String,
    /// IANA timezone name for the cron expression (e.g. "America/New_York", "Europe/London").
    /// `None` means UTC. Without this field, DST transitions can cause double-fires or misses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub agent_name: String,
    /// Legacy prompt field. Mirrors `RunTask.prompt` when `action` is
    /// `RunTask`, otherwise carries a `[<action_tag>]` placeholder so legacy
    /// log/UI surfaces continue to render something meaningful.
    pub task_prompt: String,
    pub permissions: Vec<String>, // permissions scoped to this job
    pub state: ScheduleState,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub run_count: u64,
    pub max_retries: u32,
    pub retry_count: u32,
    pub output_destination: Option<String>, // file path for results
    /// Agent that owns this schedule. Enforced on pause/resume/delete control
    /// operations from agent tools; `None` for operator/CLI-created schedules
    /// (which default-deny on agent control paths). `#[serde(default)]` keeps
    /// the field back-compat with any pre-existing persisted records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_agent_id: Option<crate::AgentID>,
    /// What fires when this schedule triggers. Source of truth.
    /// `#[serde(default)]` falls back to `RunTask` with an empty prompt so
    /// legacy rows continue to deserialize.
    #[serde(default = "default_scheduled_run_task_action")]
    pub action: OnceJobAction,
    /// How the fired result is delivered. Defaults to `Silent` for legacy rows.
    #[serde(default)]
    pub delivery: crate::delivery::DeliveryMode,
}

fn default_scheduled_run_task_action() -> OnceJobAction {
    OnceJobAction::RunTask {
        prompt: String::new(),
    }
}

impl ScheduledJob {
    /// Synthesize the legacy `task_prompt` shadow string for the given action.
    /// Used when populating the `task_prompt` SQL column (legacy `NOT NULL`).
    pub fn shadow_task_prompt(action: &OnceJobAction) -> String {
        OnceJob::shadow_task_prompt(action)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleState {
    Active,
    Paused,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub id: TaskID,
    pub name: String,
    pub agent_name: String,
    pub task_prompt: String,
    pub state: TaskState,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result: Option<serde_json::Value>,
    pub detached: bool, // if true, runs independently
    /// If this task was launched from a scheduled cron job, stores the job ID
    /// so task_completion can emit ScheduledTaskCompleted on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_job_id: Option<ScheduleID>,
}

/// What kind of scheduled item produced a `ScheduledRun`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunParentKind {
    Schedule,
    Once,
    Timer,
}

impl RunParentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Once => "once",
            Self::Timer => "timer",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "schedule" => Some(Self::Schedule),
            "once" => Some(Self::Once),
            "timer" => Some(Self::Timer),
            _ => None,
        }
    }
}

/// Names of scheduling-meta tools that must NEVER be schedulable themselves
/// (anti-recursion). Enforced at both the tool layer and the kernel parser.
/// Single source of truth — duplicating this list in multiple files would
/// cause silent bypass when a new meta-tool is added.
pub const BLOCKED_SCHEDULE_TOOL_NAMES: &[&str] = &[
    "schedule-once",
    "schedule-recurring",
    "schedule-control",
    "set-timer",
    "cancel-once-job",
    "cancel-timer",
];

/// Lifecycle state of a single run record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Fired and a backing task is executing.
    Running,
    /// Completed successfully.
    Complete,
    /// Failed (either at launch or during execution).
    Failed,
    /// Target agent missing / not connected at fire time.
    Missed,
}

impl RunState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Missed => "missed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "complete" => Some(Self::Complete),
            "failed" => Some(Self::Failed),
            "missed" => Some(Self::Missed),
            _ => None,
        }
    }
}

/// One fire of a scheduled item: cron tick, once-job trigger, or timer expiry.
/// Persisted alongside its parent (`schedule` / `once` / `timer`) so agents can
/// inspect run history and the delivery router can dispatch the result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledRun {
    pub run_id: RunID,
    pub parent_kind: RunParentKind,
    pub parent_id: ScheduleID,
    /// Cached parent name so the run record stays useful even if the parent
    /// is later deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_name: Option<String>,
    /// Agent that created the parent schedule (denormalised for fast filter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_agent_id: Option<crate::AgentID>,
    /// Background task spawned to execute this run, if any (None for direct
    /// notify/tool actions that run synchronously in the kernel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskID>,
    pub state: RunState,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Final task result (when state is Complete). Pruned eventually.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Failure reason (when state is Failed or Missed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Tool calls made during the run, for audit/debug.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
    /// How the result is to be delivered (copied from parent at fire time).
    #[serde(default)]
    pub delivery: crate::delivery::DeliveryMode,
    /// Whether the delivery has already been dispatched (idempotency guard).
    #[serde(default)]
    pub delivered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last delivery error, if delivery has been attempted and failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_error: Option<String>,
    /// Re-trigger depth — incremented on each `ViaAgent` re-fire so the
    /// kernel can hard-cap to prevent self-scheduling loops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_depth: Option<u8>,
}

#[cfg(test)]
mod once_job_action_tests {
    use super::*;

    #[test]
    fn run_task_round_trip() {
        let v = OnceJobAction::run_task("hi");
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, r#"{"type":"run_task","prompt":"hi"}"#);
        let back: OnceJobAction = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn notify_user_round_trip() {
        let v = OnceJobAction::notify_user("subj", "body", "info");
        let back: OnceJobAction =
            serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn run_tool_round_trip() {
        let v = OnceJobAction::run_tool("datetime", serde_json::json!({"format": "rfc3339"}));
        let back: OnceJobAction =
            serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn shadow_prompt_for_non_task_actions() {
        assert_eq!(
            OnceJob::shadow_task_prompt(&OnceJobAction::run_task("p")),
            "p"
        );
        assert_eq!(
            OnceJob::shadow_task_prompt(&OnceJobAction::notify_user("a", "b", "info")),
            "[notify-user]"
        );
        assert_eq!(
            OnceJob::shadow_task_prompt(&OnceJobAction::run_tool(
                "datetime",
                serde_json::Value::Null
            )),
            "[run-tool:datetime]"
        );
    }

    #[test]
    fn timer_action_run_tool_round_trip() {
        let v = TimerAction::RunTool {
            tool: "datetime".into(),
            args: serde_json::json!({}),
        };
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains(r#""type":"run_tool""#));
        let _back: TimerAction = serde_json::from_str(&json).unwrap();
    }
}
