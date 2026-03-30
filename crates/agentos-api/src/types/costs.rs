use agentos_types::AgentID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSummaryEntry {
    pub agent_id: AgentID,
    pub agent_name: String,
    pub period_start: DateTime<Utc>,
    pub tokens_used: u64,
    pub cost_usd: f64,
    pub tool_calls: u64,
}
