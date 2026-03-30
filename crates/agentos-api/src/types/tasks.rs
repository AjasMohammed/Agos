use agentos_types::TaskID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiTaskSummary {
    pub id: TaskID,
    pub agent_name: Option<String>,
    pub prompt_preview: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiTaskDetail {
    pub id: TaskID,
    pub agent_name: Option<String>,
    pub prompt: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskFilter {
    pub status: Option<String>,
    pub agent_name: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTaskRequest {
    pub prompt: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub autonomous: bool,
}
