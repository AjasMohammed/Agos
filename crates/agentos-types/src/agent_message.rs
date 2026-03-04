use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: MessageID,
    pub from: AgentID,
    pub to: MessageTarget,
    pub content: MessageContent,
    pub reply_to: Option<MessageID>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub trace_id: TraceID,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageTarget {
    Direct(AgentID),
    DirectByName(String),
    Group(GroupID),
    Broadcast,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageContent {
    Text(String),
    Structured(serde_json::Value),
    TaskDelegation {
        prompt: String,
        priority: u8,
        timeout_secs: u64,
    },
    TaskResult {
        task_id: TaskID,
        result: serde_json::Value,
    },
}
