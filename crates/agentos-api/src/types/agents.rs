use agentos_types::{AgentID, CostSnapshot, ThinkingLevel};
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
    /// Whether the connected LLM adapter will emit native image blocks for this agent.
    #[serde(default)]
    pub supports_images: bool,
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
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub thinking_level: Option<ThinkingLevel>,
    #[serde(default)]
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAgentSettingsRequest {
    pub agent_name: String,
    pub description: String,
    pub thinking_level: ThinkingLevel,
    #[serde(default)]
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub agent_name: String,
    pub permission: String,
}
