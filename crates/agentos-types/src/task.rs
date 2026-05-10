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
    /// Optional hard cap for executor iterations. When omitted, the kernel
    /// chooses a default from config based on task complexity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// If this task was triggered by an event, records the event provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_source: Option<TriggerSource>,
    /// When true, the task runs without iteration or timeout limits.
    /// Designed for long-running autonomous workflows that must run to natural
    /// completion. Limits are sourced from `[kernel.autonomous_mode]` config.
    #[serde(default)]
    pub autonomous: bool,
    /// `Some(id)` when this task was spawned by another task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskID>,
    /// How many spawn hops from a root task (root = 0, child = 1, grandchild = 2, …).
    #[serde(default)]
    pub spawn_depth: u8,
    /// True when this task is the coordinator of an agent team (`agentos team run`).
    /// Used to identify team runs in task listings without fragile prompt matching.
    #[serde(default)]
    pub is_team_coordinator: bool,
    /// When true, the task executor skips checkpoint writes for this task.
    /// Set by `--no-checkpoint` CLI flag for ephemeral one-shot tasks.
    #[serde(default)]
    pub skip_checkpoint: bool,
    /// Requested extended-thinking level for this task.
    /// Translates to `InferenceOptions::thinking_budget_tokens` at execution time.
    #[serde(default)]
    pub thinking_level: ThinkingLevel,
    /// Set when spawned via `task-spawn-async`: the agent that owns this task for
    /// ownership-based status queries across task boundaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawner_agent_id: Option<AgentID>,
    /// Optional task-scoped tool category allowlist. When `Some(list)`, only
    /// tools whose `ToolSummary.category` is in `list` are visible via the
    /// paginated manual surface (`agent-manual`, `list-tools`, `search-tools`,
    /// `describe-tool`). `None` (default) = no restriction.
    /// Sub-agent spawn must verify the requested allowlist is a subset of the
    /// parent's allowlist (narrow-only); widening is rejected as a permission
    /// escalation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_categories: Option<Vec<String>>,
}

/// Controls how much extended thinking budget the LLM is given for a task.
/// Maps to the `budget_tokens` field of the Anthropic thinking API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// No extended thinking — fastest and cheapest (default).
    #[default]
    Off,
    /// 1 024 token budget — light reasoning pass.
    Low,
    /// 8 192 token budget — balanced reasoning for moderately complex tasks.
    Medium,
    /// 32 768 token budget — deep reasoning for complex multi-step tasks.
    High,
    /// 100 000 token budget — maximum reasoning for the hardest problems.
    Max,
}

impl ThinkingLevel {
    /// Convert to the `budget_tokens` value expected by the Anthropic API.
    /// Returns `None` when thinking is disabled.
    pub fn budget_tokens(&self) -> Option<u32> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Low => Some(1_024),
            ThinkingLevel::Medium => Some(8_192),
            ThinkingLevel::High => Some(32_768),
            ThinkingLevel::Max => Some(100_000),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_task_parent_fields_default() {
        let task = AgentTask {
            parent_task_id: None,
            spawn_depth: 0,
            ..Default::default()
        };
        assert!(task.parent_task_id.is_none());
        assert_eq!(task.spawn_depth, 0);
    }

    #[test]
    fn test_thinking_level_budget_tokens() {
        assert_eq!(ThinkingLevel::Off.budget_tokens(), None);
        assert_eq!(ThinkingLevel::Low.budget_tokens(), Some(1_024));
        assert_eq!(ThinkingLevel::Medium.budget_tokens(), Some(8_192));
        assert_eq!(ThinkingLevel::High.budget_tokens(), Some(32_768));
        assert_eq!(ThinkingLevel::Max.budget_tokens(), Some(100_000));
    }

    #[test]
    fn test_thinking_level_serde_roundtrip() {
        for level in [
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Max,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            let deserialized: ThinkingLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, deserialized);
        }
    }

    #[test]
    fn test_thinking_level_defaults_when_missing_from_json() {
        // Simulate a checkpoint serialized before thinking_level was added.
        // The field should default to Off without a parse error.
        let task = AgentTask::default();
        let mut json: serde_json::Value = serde_json::to_value(&task).unwrap();
        json.as_object_mut().unwrap().remove("thinking_level");
        let restored: AgentTask = serde_json::from_value(json).unwrap();
        assert_eq!(restored.thinking_level, ThinkingLevel::Off);
    }
}

impl Default for AgentTask {
    fn default() -> Self {
        use crate::capability::CapabilityToken;
        Self {
            id: TaskID::new(),
            state: TaskState::Queued,
            agent_id: AgentID::new(),
            capability_token: CapabilityToken {
                task_id: TaskID::new(),
                agent_id: AgentID::new(),
                allowed_tools: Default::default(),
                allowed_intents: Default::default(),
                permissions: Default::default(),
                issued_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now(),
                signature: Vec::new(),
            },
            assigned_llm: None,
            priority: 0,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::ZERO,
            original_prompt: String::new(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            max_iterations: None,
            trigger_source: None,
            autonomous: false,
            parent_task_id: None,
            spawn_depth: 0,
            spawner_agent_id: None,
            tool_categories: None,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
        }
    }
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
    Waiting,   // waiting on a tool or sub-agent
    Suspended, // suspended due to budget enforcement; can be resumed
    Complete,
    Failed,
    Cancelled,
}

impl TaskState {
    /// Returns `true` if transitioning from `self` to `next` is a legal state machine move.
    ///
    /// Legal transitions:
    /// - Queued     → Running | Failed | Cancelled
    /// - Running    → Waiting | Suspended | Complete | Failed | Cancelled
    /// - Waiting    → Running | Failed | Cancelled
    /// - Suspended  → Running | Cancelled
    /// - Complete, Failed, Cancelled are terminal — no further transitions allowed.
    ///
    /// `Queued → Failed` covers tasks that fail during initialization (e.g. capability
    /// token generation fails) before ever reaching Running.
    pub fn can_transition_to(self, next: TaskState) -> bool {
        matches!(
            (self, next),
            (TaskState::Queued, TaskState::Running)
                | (TaskState::Queued, TaskState::Failed)
                | (TaskState::Queued, TaskState::Cancelled)
                | (TaskState::Running, TaskState::Waiting)
                | (TaskState::Running, TaskState::Suspended)
                | (TaskState::Running, TaskState::Complete)
                | (TaskState::Running, TaskState::Failed)
                | (TaskState::Running, TaskState::Cancelled)
                | (TaskState::Waiting, TaskState::Running)
                | (TaskState::Waiting, TaskState::Failed)
                | (TaskState::Waiting, TaskState::Cancelled)
                | (TaskState::Suspended, TaskState::Running)
                | (TaskState::Suspended, TaskState::Cancelled)
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
    pub priority: u8,
    /// True when this task is the coordinator of an agent team.
    #[serde(default)]
    pub is_team_coordinator: bool,
    /// Parent task ID if this is a sub-agent task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskID>,
    /// Spawn depth (0 = root, 1 = child, 2 = grandchild, …).
    #[serde(default)]
    pub spawn_depth: u8,
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
            max_tokens_per_day: 5_000_000,
            max_cost_usd_per_day: 50.0,
            max_tool_calls_per_day: 10_000,
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

/// Record of a single tool call during task execution, for checkpoint replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub tool_call_id: Option<String>,
    pub input_json: String,
    pub output_json: String,
    pub called_at: chrono::DateTime<chrono::Utc>,
    pub duration_ms: u64,
    pub success: bool,
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
    /// Linear forecast: estimated hours until the budget is exhausted based on
    /// the current burn rate since `period_start`. `None` when the budget is
    /// unlimited (limit == 0) or no time has elapsed yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forecast_exhaustion_hours: Option<f64>,
}
