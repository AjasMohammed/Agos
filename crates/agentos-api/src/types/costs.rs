use agentos_types::AgentID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CostSummaryEntry {
    #[schema(value_type = String)]
    pub agent_id: AgentID,
    pub agent_name: String,
    pub period_start: DateTime<Utc>,
    pub tokens_used: u64,
    pub cost_usd: f64,
    pub tool_calls: u64,
    /// Percentage of the daily cost budget consumed (0 when unbudgeted).
    #[serde(default)]
    pub cost_pct: f64,
    /// Percentage of the daily token budget consumed (0 when unbudgeted).
    #[serde(default)]
    pub tokens_pct: f64,
    /// Percentage of the daily tool-call budget consumed (0 when unbudgeted).
    #[serde(default)]
    pub tool_calls_pct: f64,
    /// Linear forecast: estimated hours until the budget is exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forecast_exhaustion_hours: Option<f64>,
    /// The active budget, when one is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<CostBudget>,
}

/// A per-agent daily budget snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CostBudget {
    pub max_cost_usd_per_day: f64,
    pub max_tokens_per_day: u64,
    pub spent_today_usd: f64,
    /// Percentage of the cost budget consumed.
    pub pct: f64,
}
