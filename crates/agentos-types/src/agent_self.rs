use crate::ids::{AgentID, SubscriptionID};
use crate::registry_query::TaskIntrospectionSummary;
use serde::{Deserialize, Serialize};

/// A snapshot of an agent's own state, returned by the `agent-self` tool.
///
/// All fields reflect the agent's current perspective at the time of the call.
/// The agent calling this tool can only view its own data — it cannot inspect
/// other agents using this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSelfView {
    pub agent_id: AgentID,
    /// Human-readable agent name, or empty string if not registered.
    pub name: String,
    /// Current lifecycle status: "online" | "idle" | "busy" | "offline".
    pub status: String,
    /// Granted permission entries formatted as "resource:rwx" (e.g. "fs.user_data:rw").
    pub permissions: Vec<String>,
    /// Deny-list entries — resource patterns that are explicitly blocked.
    pub deny_entries: Vec<String>,
    /// Current budget consumption. `None` when cost tracking is not wired into
    /// the execution context (e.g. tests, lightweight pipelines).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetSummary>,
    /// Names of every tool available to this agent in the current runner.
    /// Empty when the runner has not been initialised with self-view support.
    pub tools: Vec<String>,
    /// Active event subscriptions. Empty when no subscription query interface
    /// is available in the current execution context.
    pub subscriptions: Vec<SubscriptionSummary>,
    /// Active (queued / running / waiting) tasks for this agent, newest first.
    pub active_tasks: Vec<TaskIntrospectionSummary>,
}

/// Budget consumption summary at the point the `agent-self` tool was called.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSummary {
    /// Input tokens consumed in the current billing period.
    pub input_tokens_used: u64,
    /// Output tokens consumed in the current billing period.
    pub output_tokens_used: u64,
    /// Estimated USD cost in the current billing period.
    pub total_cost_usd: f64,
    /// Hard USD limit per day, if configured. `None` = unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_limit_usd: Option<f64>,
    /// Remaining USD before the hard limit is hit. `None` = unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_usd: Option<f64>,
}

/// Summary of a single active event subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionSummary {
    pub subscription_id: SubscriptionID,
    /// Human-readable event type or category string.
    pub event_type: String,
    pub enabled: bool,
}
