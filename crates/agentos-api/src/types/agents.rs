use agentos_types::{AgentID, CostSnapshot};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAgentSummary {
    pub id: AgentID,
    pub name: String,
    pub provider: String,
    pub model: String,
    pub status: String,
    pub roles: Vec<String>,
    pub connected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAgentDetail {
    pub summary: ApiAgentSummary,
    pub permissions: Vec<String>,
    pub recent_tasks: Vec<super::tasks::ApiTaskSummary>,
    pub cost_snapshot: Option<CostSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectAgentRequest {
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub agent_name: String,
    pub permission: String,
}
