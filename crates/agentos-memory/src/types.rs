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

    pub fn parse(s: &str) -> Option<Self> {
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

impl std::str::FromStr for EpisodeType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| format!("Unknown episode type: {s}"))
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

/// A single step in a stored procedure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureStep {
    /// Execution order (0-indexed).
    pub order: usize,
    /// Human-readable action description.
    pub action: String,
    /// Tool name to invoke for this step (if applicable).
    pub tool: Option<String>,
    /// What success looks like for this step.
    pub expected_outcome: Option<String>,
}

/// A stored procedure representing a learned skill or SOP.
///
/// Procedures are distilled from repeated episodic patterns (Phase 7 consolidation)
/// or created explicitly by agents (Phase 8 memory self-management).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Procedure {
    /// UUID primary key.
    pub id: String,
    /// Short descriptive name, e.g. "deploy-to-production".
    pub name: String,
    /// What this procedure accomplishes.
    pub description: String,
    /// Conditions that must hold before execution.
    pub preconditions: Vec<String>,
    /// Ordered steps.
    pub steps: Vec<ProcedureStep>,
    /// Expected outcomes after successful execution.
    pub postconditions: Vec<String>,
    /// Times this procedure led to a successful outcome.
    pub success_count: u32,
    /// Times this procedure led to a failure.
    pub failure_count: u32,
    /// Episodic entry IDs this procedure was distilled from.
    pub source_episodes: Vec<String>,
    /// Owning agent (None = globally available).
    pub agent_id: Option<AgentID>,
    /// Free-form tags for categorization.
    pub tags: Vec<String>,
    /// When this procedure was first created.
    pub created_at: DateTime<Utc>,
    /// When this procedure was last modified.
    pub updated_at: DateTime<Utc>,
}

/// Result of a hybrid procedural search with score breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureSearchResult {
    pub procedure: Procedure,
    /// Cosine similarity between query embedding and procedure embedding.
    pub semantic_score: f32,
    /// BM25 / FTS5 rank score (negated — higher is better).
    pub fts_score: f32,
    /// Reciprocal Rank Fusion score (70% semantic + 30% FTS).
    pub rrf_score: f32,
}
