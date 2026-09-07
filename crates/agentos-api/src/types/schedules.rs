//! DTOs for the schedules (cron) automation surface.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The wire vocabulary for a schedule's state (same rationale as
/// [`crate::types::ApiTaskStatus`] — a closed set the panel can be typed against).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ApiScheduleState {
    /// Cron schedules.
    Active,
    Paused,
    Disabled,
    /// One-shot jobs and timers.
    Pending,
    Fired,
    Cancelled,
}

impl ApiScheduleState {
    /// The wire spelling (matches the serde representation).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Disabled => "disabled",
            Self::Pending => "pending",
            Self::Fired => "fired",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for ApiScheduleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&agentos_types::ScheduleState> for ApiScheduleState {
    fn from(s: &agentos_types::ScheduleState) -> Self {
        match s {
            agentos_types::ScheduleState::Active => Self::Active,
            agentos_types::ScheduleState::Paused => Self::Paused,
            agentos_types::ScheduleState::Disabled => Self::Disabled,
        }
    }
}

/// A single scheduled entry as returned by `GET /api/v1/schedules` — a
/// recurring cron job, a one-shot once-job, or an in-memory timer.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiScheduleSummary {
    pub id: String,
    pub name: String,
    pub agent_name: String,
    /// One of `cron` (recurring) | `once` (one-shot job) | `timer` (in-memory countdown).
    pub kind: String,
    /// Cron expression; `null` for `once` and `timer` kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// `active` | `paused` | `disabled` for cron; `pending` for once/timer.
    pub state: ApiScheduleState,
    pub prompt: String,
    pub run_count: u64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    /// Delivery mode tag — `silent` | `direct` | `via_agent`.
    pub delivery_mode: String,
}

/// Request body for `POST /api/v1/schedules`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateScheduleRequest {
    pub name: String,
    pub agent_name: String,
    pub cron: String,
    pub prompt: String,
    /// Accepted for forward-compatibility; v1 always creates Silent delivery.
    #[serde(default)]
    pub delivery_mode: Option<String>,
}

/// Request body for `POST /api/v1/schedules/preview`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CronPreviewRequest {
    pub cron: String,
    /// Number of upcoming fire times to compute (default 5, capped at 50).
    #[serde(default)]
    pub count: Option<usize>,
}

/// Response for `POST /api/v1/schedules/preview`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CronPreviewResponse {
    /// RFC3339 timestamps of the next fire times.
    pub next_runs: Vec<String>,
}

/// A single recorded fire of a schedule (`GET /api/v1/schedules/{id}/runs`).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiScheduleRun {
    pub run_id: String,
    pub fired_at: Option<DateTime<Utc>>,
    /// One of `running` | `complete` | `failed` | `missed`.
    pub status: String,
    pub task_id: Option<String>,
}
