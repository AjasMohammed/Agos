use agentos_types::{AgentID, TaskID};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Canonical structured state for a task's active intent.
///
/// This is designed to preserve task meaning even when raw transcript history is
/// summarized, compacted, or only partially replayed into the next prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentLedger {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskID>,

    pub goal: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hard_constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_plan: Vec<PlanStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_step_id: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<DecisionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_facts: Vec<FactRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<BlockerRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<QuestionRecord>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegated_tasks: Vec<DelegationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_tool_chain: Vec<ToolChainRecord>,

    pub latest_user_intent: String,
    pub last_meaningful_progress_at: DateTime<Utc>,
    #[serde(default)]
    pub version: u64,
}

impl IntentLedger {
    /// Bootstrap a minimal ledger directly from the original task prompt.
    pub fn from_task_prompt(
        task_id: TaskID,
        agent_id: AgentID,
        parent_task_id: Option<TaskID>,
        prompt: impl Into<String>,
    ) -> Self {
        let prompt = prompt.into();
        let initial_step_id = "step-1".to_string();
        Self {
            task_id,
            agent_id,
            parent_task_id,
            goal: prompt.clone(),
            success_criteria: Vec::new(),
            hard_constraints: Vec::new(),
            assumptions: Vec::new(),
            current_plan: vec![PlanStep {
                id: initial_step_id.clone(),
                description: format!(
                    "Complete the requested task: {}",
                    prompt.chars().take(160).collect::<String>()
                ),
                status: PlanStepStatus::InProgress,
            }],
            active_step_id: Some(initial_step_id),
            decisions: Vec::new(),
            discovered_facts: Vec::new(),
            blockers: Vec::new(),
            open_questions: Vec::new(),
            delegated_tasks: Vec::new(),
            active_tool_chain: Vec::new(),
            latest_user_intent: prompt,
            last_meaningful_progress_at: Utc::now(),
            version: 1,
        }
    }

    /// Create a compact digest suitable for prompt injection or logging.
    pub fn digest(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("Goal: {}", self.goal));

        if !self.success_criteria.is_empty() {
            lines.push(format!(
                "Success criteria: {}",
                self.success_criteria.join(" | ")
            ));
        }
        if !self.hard_constraints.is_empty() {
            lines.push(format!(
                "Constraints: {}",
                self.hard_constraints.join(" | ")
            ));
        }
        if let Some(active_step) = self
            .active_step_id
            .as_deref()
            .and_then(|id| self.current_plan.iter().find(|s| s.id == id))
        {
            lines.push(format!("Active step: {}", active_step.description));
        }
        if !self.decisions.is_empty() {
            let decisions = self
                .decisions
                .iter()
                .rev()
                .take(2)
                .map(|d| d.summary.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
            lines.push(format!("Recent decisions: {decisions}"));
        }
        if !self.blockers.is_empty() {
            let blockers = self
                .blockers
                .iter()
                .map(|b| b.summary.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
            lines.push(format!("Blockers: {blockers}"));
        }
        if !self.open_questions.is_empty() {
            let questions = self
                .open_questions
                .iter()
                .map(|q| q.question.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
            lines.push(format!("Open questions: {questions}"));
        }
        if !self.delegated_tasks.is_empty() {
            let pending = self
                .delegated_tasks
                .iter()
                .filter(|d| !d.completed)
                .map(|d| d.summary.as_str())
                .collect::<Vec<_>>();
            if !pending.is_empty() {
                lines.push(format!("Delegations pending: {}", pending.join(" | ")));
            }
        }

        lines.join("\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rationale: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactRecord {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockerRecord {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionRecord {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRecord {
    pub child_task_id: TaskID,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChainRecord {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}
