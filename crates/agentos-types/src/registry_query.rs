use crate::ids::{AgentID, TaskID};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lightweight agent summary returned by the `agent-list` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id: AgentID,
    pub name: String,
    /// Human-readable status string: "online" | "idle" | "busy" | "offline" etc.
    pub status: String,
    pub registered_at: DateTime<Utc>,
}

/// Thin query interface for the agent registry.
/// Defined in `agentos-types` so `agentos-tools` can reference it
/// without creating a circular dependency on `agentos-kernel`.
pub trait AgentRegistryQuery: Send + Sync {
    /// Return all registered agents as lightweight summaries.
    fn list_agents(&self) -> Vec<AgentSummary>;

    /// Return a single agent by ID, or None if not found.
    fn get_agent(&self, id: &AgentID) -> Option<AgentSummary>;
}

/// Lightweight task summary returned by task introspection tools.
/// Named `TaskIntrospectionSummary` to avoid conflict with the existing
/// `TaskSummary` in `agentos_types::task` which serves scheduler display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIntrospectionSummary {
    pub id: TaskID,
    pub agent_id: AgentID,
    /// First 100 chars of the original prompt.
    pub description: String,
    /// "queued" | "running" | "waiting" | "complete" | "failed" | "cancelled"
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
}

/// Thin query interface for the task store / scheduler.
pub trait TaskQuery: Send + Sync {
    /// Return a single task by ID, or None if not found.
    fn get_task(&self, id: &TaskID) -> Option<TaskIntrospectionSummary>;

    /// Return tasks for a specific agent, newest first.
    fn list_tasks_for_agent(
        &self,
        agent_id: &AgentID,
        limit: usize,
    ) -> Vec<TaskIntrospectionSummary>;

    /// Return all active (queued + running + waiting) tasks.
    fn list_active_tasks(&self, limit: usize) -> Vec<TaskIntrospectionSummary>;
}

/// Snapshot implementation of `AgentRegistryQuery`.
/// Built once at ToolExecutionContext creation time; immutable thereafter.
#[derive(Clone)]
pub struct AgentRegistrySnapshot {
    agents: Vec<AgentSummary>,
}

impl AgentRegistrySnapshot {
    pub fn new(agents: Vec<AgentSummary>) -> Self {
        Self { agents }
    }
}

impl AgentRegistryQuery for AgentRegistrySnapshot {
    fn list_agents(&self) -> Vec<AgentSummary> {
        self.agents.clone()
    }

    fn get_agent(&self, id: &AgentID) -> Option<AgentSummary> {
        self.agents.iter().find(|a| &a.id == id).cloned()
    }
}

/// Snapshot implementation of `TaskQuery`.
/// Built once at ToolExecutionContext creation time; immutable thereafter.
#[derive(Clone)]
pub struct TaskSnapshot {
    tasks: Vec<TaskIntrospectionSummary>,
}

impl TaskSnapshot {
    pub fn new(tasks: Vec<TaskIntrospectionSummary>) -> Self {
        Self { tasks }
    }
}

impl TaskQuery for TaskSnapshot {
    fn get_task(&self, id: &TaskID) -> Option<TaskIntrospectionSummary> {
        self.tasks.iter().find(|t| &t.id == id).cloned()
    }

    fn list_tasks_for_agent(
        &self,
        agent_id: &AgentID,
        limit: usize,
    ) -> Vec<TaskIntrospectionSummary> {
        let mut results: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| &t.agent_id == agent_id)
            .cloned()
            .collect();
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit.min(100));
        results
    }

    fn list_active_tasks(&self, limit: usize) -> Vec<TaskIntrospectionSummary> {
        let active = ["queued", "running", "waiting"];
        let mut results: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| active.contains(&t.status.as_str()))
            .cloned()
            .collect();
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit.min(100));
        results
    }
}

/// Lightweight escalation summary returned by the `escalation-status` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationSummary {
    pub id: u64,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub reason: String,
    pub context_summary: String,
    pub decision_point: String,
    pub options: Vec<String>,
    pub urgency: String,
    pub blocking: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub resolved: bool,
    pub resolution: Option<String>,
}

/// Thin query interface for the escalation manager.
pub trait EscalationQuery: Send + Sync {
    /// Return pending (unresolved) escalations for a specific agent.
    fn list_pending_for_agent(&self, agent_id: &AgentID) -> Vec<EscalationSummary>;

    /// Return a single escalation by ID, or None if not found.
    fn get_escalation(&self, id: u64) -> Option<EscalationSummary>;
}

/// Snapshot implementation of `EscalationQuery`.
#[derive(Clone)]
pub struct EscalationSnapshot {
    escalations: Vec<EscalationSummary>,
}

impl EscalationSnapshot {
    pub fn new(escalations: Vec<EscalationSummary>) -> Self {
        Self { escalations }
    }
}

impl EscalationQuery for EscalationSnapshot {
    fn list_pending_for_agent(&self, agent_id: &AgentID) -> Vec<EscalationSummary> {
        self.escalations
            .iter()
            .filter(|e| &e.agent_id == agent_id && !e.resolved)
            .cloned()
            .collect()
    }

    fn get_escalation(&self, id: u64) -> Option<EscalationSummary> {
        self.escalations.iter().find(|e| e.id == id).cloned()
    }
}
