use crate::capability::CapabilityToken;
use crate::ids::*;
use crate::intent::IntentMessage;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A single unit of work assigned to an LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: TaskID,
    pub state: TaskState,
    pub agent_id: AgentID,
    pub capability_token: CapabilityToken,
    pub assigned_llm: Option<AgentID>,
    pub priority: u8,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When this task started executing (transitioned to Running).
    /// Used by the timeout checker to measure elapsed execution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub timeout: Duration,
    pub original_prompt: String,
    pub history: Vec<IntentMessage>,
    pub parent_task: Option<TaskID>,
    /// Optional hints about how this task should be reasoned about.
    #[serde(default)]
    pub reasoning_hints: Option<TaskReasoningHints>,
    /// If this task was triggered by an event, records the event provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_source: Option<TriggerSource>,
}

/// Provenance data for a task that was triggered by an OS event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerSource {
    pub event_id: crate::ids::EventID,
    pub event_type: crate::event::EventType,
    pub subscription_id: crate::ids::SubscriptionID,
    pub chain_depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Queued,
    Running,
    Waiting, // waiting on a tool or sub-agent
    Complete,
    Failed,
    Cancelled,
}

impl TaskState {
    /// Returns `true` if transitioning from `self` to `next` is a legal state machine move.
    ///
    /// Legal transitions:
    /// - Queued   → Running | Cancelled
    /// - Running  → Waiting | Complete | Failed | Cancelled
    /// - Waiting  → Running | Failed | Cancelled
    /// - Complete, Failed, Cancelled are terminal — no further transitions allowed.
    pub fn can_transition_to(self, next: TaskState) -> bool {
        matches!(
            (self, next),
            (TaskState::Queued, TaskState::Running)
                | (TaskState::Queued, TaskState::Cancelled)
                | (TaskState::Running, TaskState::Waiting)
                | (TaskState::Running, TaskState::Complete)
                | (TaskState::Running, TaskState::Failed)
                | (TaskState::Running, TaskState::Cancelled)
                | (TaskState::Waiting, TaskState::Running)
                | (TaskState::Waiting, TaskState::Failed)
                | (TaskState::Waiting, TaskState::Cancelled)
        )
    }

    /// Attempt to transition to `next`. Returns an error string if the transition is illegal.
    pub fn transition(&mut self, next: TaskState) -> Result<(), String> {
        if self.can_transition_to(next) {
            *self = next;
            Ok(())
        } else {
            Err(format!(
                "invalid task state transition: {:?} → {:?}",
                self, next
            ))
        }
    }
}

/// Summary of a task for display purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: TaskID,
    pub state: TaskState,
    pub agent_id: AgentID,
    pub prompt_preview: String, // first 100 chars of prompt
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tool_calls: u32,
    pub tokens_used: u64,
}

/// Hints for the scheduler and executor about how to handle task reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReasoningHints {
    /// Estimated complexity of this task.
    pub estimated_complexity: ComplexityLevel,
    /// Suggested maximum number of LLM turns before the task should yield.
    pub preferred_turns: Option<u32>,
    /// How sensitive this task is to preemption/timeout.
    pub preemption_sensitivity: PreemptionLevel,
}

/// Estimated complexity of a task, used for scheduling hints.
/// Variant order is significant: Low < Medium < High (derived Ord).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ComplexityLevel {
    /// Simple lookup or single-step operation.
    Low,
    /// Multi-step reasoning or moderate tool use.
    Medium,
    /// Complex multi-agent coordination or deep analysis.
    High,
}

/// How sensitive a task is to being preempted or timed out.
/// Variant order is significant: Low < Normal < High (derived Ord).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PreemptionLevel {
    /// Can be safely interrupted at any point.
    Low,
    /// Prefer not to interrupt mid-reasoning.
    Normal,
    /// Should be given extra time; interruption may lose significant work.
    High,
}

/// Budget configuration for an agent. Enforced by the kernel's cost accumulator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBudget {
    /// Maximum tokens (input + output) per day. 0 = unlimited.
    pub max_tokens_per_day: u64,
    /// Maximum USD spend per day. 0.0 = unlimited.
    pub max_cost_usd_per_day: f64,
    /// Maximum tool calls per day. 0 = unlimited.
    pub max_tool_calls_per_day: u64,
    /// Percentage at which to emit a warning (0-100).
    pub warn_at_pct: u8,
    /// Percentage at which to pause the agent (0-100).
    pub pause_at_pct: u8,
    /// Action to take when hard limit is hit.
    pub on_hard_limit: BudgetAction,
    /// Optional cheaper model to switch to when `pause_at_pct` is reached.
    /// When set, instead of pausing the task the kernel routes subsequent LLM
    /// calls to this model (e.g. Haiku instead of Sonnet) and continues.
    #[serde(default)]
    pub downgrade_model: Option<ModelDowngradeTier>,
    /// Optional allowlist of permitted model names. If non-empty, only models
    /// in this list may be used for inference. Empty = all models allowed.
    #[serde(default)]
    pub allowed_models: Vec<String>,
    /// Maximum wall-clock time in seconds for a single task. 0 = unlimited.
    #[serde(default)]
    pub max_wall_time_seconds: u64,
}

impl Default for AgentBudget {
    fn default() -> Self {
        Self {
            max_tokens_per_day: 500_000,
            max_cost_usd_per_day: 5.0,
            max_tool_calls_per_day: 200,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: Vec::new(),
            max_wall_time_seconds: 0,
        }
    }
}

/// What to do when a hard budget limit is hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetAction {
    /// Suspend the agent's running tasks (can be resumed after budget reset).
    Suspend,
    /// Only notify — don't stop execution.
    NotifyOnly,
    /// Kill the task immediately.
    Kill,
}

/// Model downgrade tier — a cheaper model to fall back to when budget is near.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDowngradeTier {
    /// Model name to switch to when approaching the pause threshold.
    pub model: String,
    /// Provider for the downgrade model (must match the agent's current provider).
    pub provider: String,
}

/// Snapshot of an agent's current cost accumulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSnapshot {
    pub agent_id: crate::ids::AgentID,
    pub agent_name: String,
    pub period_start: chrono::DateTime<chrono::Utc>,
    pub tokens_used: u64,
    pub cost_usd: f64,
    pub tool_calls: u64,
    pub budget: AgentBudget,
    pub tokens_pct: f64,
    pub cost_pct: f64,
    pub tool_calls_pct: f64,
}
