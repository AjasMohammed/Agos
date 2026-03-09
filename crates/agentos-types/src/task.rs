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
    pub timeout: Duration,
    pub original_prompt: String,
    pub history: Vec<IntentMessage>,
    pub parent_task: Option<TaskID>,
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
