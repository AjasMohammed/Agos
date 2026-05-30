use agentos_types::TaskID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiTaskSummary {
    #[schema(value_type = String)]
    pub id: TaskID,
    pub agent_name: Option<String>,
    pub prompt_preview: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiTaskDetail {
    #[schema(value_type = String)]
    pub id: TaskID,
    pub agent_name: Option<String>,
    pub prompt: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TaskFilter {
    pub status: Option<String>,
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
