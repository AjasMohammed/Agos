//! DTOs for the read-only skills library.

use serde::{Deserialize, Serialize};

/// A skill in the registry, list view.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiSkillSummary {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub trust_tier: String,
    #[serde(default)]
    pub roles: Vec<String>,
    /// Cron schedule for autonomous runs, if any.
    #[serde(default)]
    pub schedule: Option<String>,
    /// Kernel events that trigger the skill, if any.
    #[serde(default)]
    pub events: Vec<String>,
}

/// Full skill detail including required tools/permissions, budget, and prompt.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiSkillDetail {
    pub summary: ApiSkillSummary,
    pub license: Option<String>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub tools_required: Vec<String>,
    pub tools_optional: Vec<String>,
    pub permissions_required: Vec<String>,
    pub max_cost_per_run: f64,
    pub max_tokens_per_run: u64,
    /// Full system-prompt text for the skill's agent.
    pub system_prompt: String,
}
