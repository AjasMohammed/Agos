use super::agents::ApiAgentSummary;
use super::audit::AuditEntrySummary;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub uptime_secs: u64,
    pub agent_count: usize,
    pub task_count: usize,
    pub tool_count: usize,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSummary {
    pub agent_count: usize,
    pub online_agents: Vec<ApiAgentSummary>,
    pub task_counts: TaskCounts,
    pub tool_count: usize,
    pub uptime_secs: u64,
    pub recent_audit: Vec<AuditEntrySummary>,
    pub background_task_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCounts {
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub total: usize,
}
