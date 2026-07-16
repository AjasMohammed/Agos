use agentos_types::{AgentID, CostSnapshot, ThinkingLevel};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiAgentSummary {
    #[schema(value_type = String)]
    pub id: AgentID,
    pub name: String,
    pub provider: String,
    pub model: String,
    pub status: String,
    pub roles: Vec<String>,
    pub connected_at: DateTime<Utc>,
    /// Last time the agent acted or was woken (by a task, message, or heartbeat).
    /// Drives the liveness indicator; also what the heartbeat scheduler reads to
    /// decide which idle agents are due for a wakeup.
    #[serde(default = "epoch")]
    pub last_active: DateTime<Utc>,
    /// Whether the connected LLM adapter will emit native image blocks for this agent.
    #[serde(default)]
    pub supports_images: bool,
}

/// Serde default for `last_active` on older payloads that predate the field.
fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiAgentDetail {
    pub summary: ApiAgentSummary,
    pub permissions: Vec<String>,
    pub recent_tasks: Vec<super::tasks::ApiTaskSummary>,
    #[schema(value_type = Option<Object>)]
    pub cost_snapshot: Option<CostSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
    #[schema(value_type = String)]
    pub thinking_level: Option<ThinkingLevel>,
    #[serde(default)]
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UpdateAgentSettingsRequest {
    pub agent_name: String,
    pub description: String,
    #[schema(value_type = String)]
    pub thinking_level: ThinkingLevel,
    #[serde(default)]
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PermissionRequest {
    pub agent_name: String,
    pub permission: String,
}
