use agentos_types::ContentPart;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSessionSummary {
    pub id: String,
    pub agent_name: String,
    pub created_at: DateTime<Utc>,
    pub message_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub session_id: String,
    pub agent_name: String,
    pub message: String,
    #[serde(default)]
    pub history: Vec<(String, String)>,
    /// Multimodal segments for the latest user turn. When empty, the kernel uses `message` as plain text.
    #[serde(default)]
    pub parts: Vec<ContentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: String,
    pub tool_calls: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub agent_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageRequest {
    pub message: String,
    #[serde(default)]
    pub agent_name: Option<String>,
}
