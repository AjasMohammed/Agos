use crate::{AgentID, TaskID};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Full execution trace for a single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTrace {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: String,
    pub prompt_preview: String,
    pub iterations: Vec<IterationTrace>,
    pub snapshot_ids: Vec<String>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
}

/// One LLM inference iteration with all subsequent tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationTrace {
    pub iteration: u32,
    pub started_at: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub stop_reason: String,
    pub tool_calls: Vec<ToolCallTrace>,
    pub snapshot_id: Option<String>,
}

/// One tool invocation within an iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallTrace {
    pub tool_name: String,
    pub input_json: serde_json::Value,
    pub output_json: Option<serde_json::Value>,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub permission_check: PermissionCheckTrace,
    pub injection_score: Option<f32>,
    pub snapshot_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionCheckTrace {
    pub granted: bool,
    pub deny_reason: Option<String>,
}

/// Lightweight summary for listing traces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTraceSummary {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: String,
    pub prompt_preview: String,
    pub iteration_count: u32,
    pub tool_call_count: u32,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
}

impl TaskTrace {
    pub fn summary(&self) -> TaskTraceSummary {
        let tool_call_count = self
            .iterations
            .iter()
            .map(|i| i.tool_calls.len() as u32)
            .sum();
        TaskTraceSummary {
            task_id: self.task_id,
            agent_id: self.agent_id,
            started_at: self.started_at,
            finished_at: self.finished_at,
            status: self.status.clone(),
            prompt_preview: self.prompt_preview.clone(),
            iteration_count: self.iterations.len() as u32,
            tool_call_count,
            total_tokens: self.total_input_tokens + self.total_output_tokens,
            total_cost_usd: self.total_cost_usd,
        }
    }
}
