use agentos_types::{AgentID, TaskID, TraceID};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Type of episode stored in episodic memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EpisodeType {
    Intent,
    ToolCall,
    ToolResult,
    LLMResponse,
    AgentMessage,
    UserPrompt,
    SystemEvent,
}

impl EpisodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EpisodeType::Intent => "intent",
            EpisodeType::ToolCall => "tool_call",
            EpisodeType::ToolResult => "tool_result",
            EpisodeType::LLMResponse => "llm_response",
            EpisodeType::AgentMessage => "agent_message",
            EpisodeType::UserPrompt => "user_prompt",
            EpisodeType::SystemEvent => "system_event",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "intent" => Some(EpisodeType::Intent),
            "tool_call" => Some(EpisodeType::ToolCall),
            "tool_result" => Some(EpisodeType::ToolResult),
            "llm_response" => Some(EpisodeType::LLMResponse),
            "agent_message" => Some(EpisodeType::AgentMessage),
            "user_prompt" => Some(EpisodeType::UserPrompt),
            "system_event" => Some(EpisodeType::SystemEvent),
            _ => None,
        }
    }
}

/// A stored entry in episodic memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodicEntry {
    pub id: i64,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub entry_type: EpisodeType,
    pub content: String,
    pub summary: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub timestamp: DateTime<Utc>,
    pub trace_id: TraceID,
}

/// Represents the top-level parent wrapper for a piece of semantic knowledge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String, // UUID
    pub agent_id: Option<AgentID>,
    pub key: String,
    pub full_content: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

/// For the underlying chunks attached to a `MemoryEntry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryChunk {
    pub id: String,        // UUID per chunk
    pub memory_id: String, // Parent entry UUID
    pub chunk_index: usize,
    pub content: String,
}

/// Query parameters for episodic/semantic recall operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallQuery {
    pub query: String,
    pub top_k: usize,
    pub min_score: Option<f32>,
}

/// Result of a hybrid semantic search holding score factors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResult {
    pub entry: MemoryEntry,
    pub chunk: MemoryChunk,
    pub semantic_score: f32, // Cosine similarity
    pub fts_score: f32,      // BM25 or raw rank
    pub rrf_score: f32,      // Fused rank score
}
