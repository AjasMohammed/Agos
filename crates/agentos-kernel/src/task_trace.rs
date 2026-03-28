// Re-export shared trace types from agentos-types so kernel internals can use them
// without re-defining them (avoids circular dependency with agentos-bus).
pub use agentos_types::task_trace::{
    IterationTrace, PermissionCheckTrace, TaskTrace, TaskTraceSummary, ToolCallTrace,
};

use agentos_types::AgentID;
use chrono::{DateTime, Utc};

// ── In-memory accumulator ─────────────────────────────────────────────────────

pub(crate) struct ActiveTrace {
    pub agent_id: AgentID,
    pub started_at: DateTime<Utc>,
    pub prompt_preview: String,
    pub completed_iterations: Vec<IterationTrace>,
    /// Current in-progress iteration, set by `begin_iteration`.
    pub current_iter: Option<IterationBuilder>,
    pub snapshot_ids: Vec<String>,
    pub total_cost_usd: f64,
}

pub(crate) struct IterationBuilder {
    pub iteration: u32,
    pub started_at: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub stop_reason: String,
    pub tool_calls: Vec<ToolCallTrace>,
    pub snapshot_id: Option<String>,
}

impl IterationBuilder {
    pub fn build(self) -> IterationTrace {
        IterationTrace {
            iteration: self.iteration,
            started_at: self.started_at,
            model: self.model,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            stop_reason: self.stop_reason,
            tool_calls: self.tool_calls,
            snapshot_id: self.snapshot_id,
        }
    }
}
