use agentos_types::TaskID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The wire vocabulary for a task's lifecycle state.
///
/// A typed enum, not a bare `String`: the panel's filter chips used to send
/// `completed` while the API says `complete`, which silently returned zero rows
/// for months. As an enum it lands in `openapi.json` as a closed set, the
/// generated TypeScript is a union, and that class of drift becomes a compile
/// error on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ApiTaskStatus {
    Queued,
    Running,
    Waiting,
    Suspended,
    Complete,
    Failed,
    Cancelled,
}

impl ApiTaskStatus {
    /// The wire spelling (matches the serde representation).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Suspended => "suspended",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a query-string value. `None` for anything outside the vocabulary,
    /// so a stale `?status=completed` is rejected rather than silently matching
    /// nothing.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "waiting" => Some(Self::Waiting),
            "suspended" => Some(Self::Suspended),
            "complete" => Some(Self::Complete),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

impl std::fmt::Display for ApiTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&agentos_types::TaskState> for ApiTaskStatus {
    fn from(s: &agentos_types::TaskState) -> Self {
        use agentos_types::TaskState as T;
        match s {
            T::Queued => Self::Queued,
            T::Running => Self::Running,
            T::Waiting => Self::Waiting,
            T::Suspended => Self::Suspended,
            T::Complete => Self::Complete,
            T::Failed => Self::Failed,
            T::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiTaskSummary {
    #[schema(value_type = String)]
    pub id: TaskID,
    pub agent_name: Option<String>,
    pub prompt_preview: String,
    pub status: ApiTaskStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Failure reason when `status == "failed"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiTaskDetail {
    #[schema(value_type = String)]
    pub id: TaskID,
    pub agent_name: Option<String>,
    pub prompt: String,
    pub status: ApiTaskStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Event type that triggered this task, if it was created by an event
    /// subscription (e.g. `DiskSpaceLow`). Absent for user/scheduled tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_event_type: Option<String>,
    /// Failure reason when `status == "failed"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The agent's final answer when `status == "complete"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TaskFilter {
    pub status: Option<ApiTaskStatus>,
    pub agent_name: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunTaskRequest {
    pub prompt: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub autonomous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiCheckpointSummary {
    pub task_id: String,
    pub created_at: DateTime<Utc>,
    pub iteration: u32,
    pub tool_calls: u32,
}
