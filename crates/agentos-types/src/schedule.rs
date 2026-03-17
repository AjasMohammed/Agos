use crate::*;
use serde::{Deserialize, Serialize};

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
