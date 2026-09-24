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

/// Lifecycle status of a memory entry. Decay demotes entries through
/// `Active → Stale → Archived`; transitions are reversible and entries are
/// never hard-deleted by the lifecycle engine itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStatus {
    #[default]
    Active,
    Stale,
    Archived,
}

impl MemoryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryStatus::Active => "active",
            MemoryStatus::Stale => "stale",
            MemoryStatus::Archived => "archived",
        }
    }

    /// Unknown strings map to `Active` so a corrupt status can never hide an
    /// entry from retrieval (fail-open to visible).
    pub fn parse(s: &str) -> Self {
        match s {
            "stale" => MemoryStatus::Stale,
            "archived" => MemoryStatus::Archived,
            _ => MemoryStatus::Active,
        }
    }
}

/// Default confidence assigned to newly created memories.
pub fn default_confidence() -> f32 {
    0.6
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
    /// When this entry was last injected into an agent context (None = never).
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    /// Times this entry was injected into an agent context.
    #[serde(default)]
    pub use_count: u32,
    /// Lifecycle confidence (0..1), reinforced on use and decayed over time.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// Lifecycle status; non-active entries are excluded from default search.
    #[serde(default)]
    pub status: MemoryStatus,
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

/// A parameter a procedure accepts at run time.
///
/// Bound into the execution template context as `inputs.<name>`, so a step
/// payload references it as `{{inputs.text}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureInput {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    /// Used when the caller omits a non-required input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// A single step in a stored procedure.
///
/// A step is prose (`action` only) or a real call (`tool` + `input`). A
/// procedure made entirely of the latter is executable; see
/// [`Procedure::is_executable`].
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
    /// Tool payload template. `{{inputs.x}}` binds a declared input;
    /// `{{var}}` binds an earlier step's `output_var`.
    ///
    /// A field whose value is EXACTLY one binding takes that value's own JSON
    /// type; a binding among surrounding text is stringified and interpolated.
    /// Either way the payload is rendered by walking the JSON, so a bound value
    /// cannot change the payload's shape.
    ///
    /// `#[serde(default)]`: procedures written before executable steps existed
    /// deserialize with `None` and stay prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// Name this step's output is bound to for later steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_var: Option<String>,
}

/// Longest procedure name / input name / output_var accepted.
const MAX_IDENTIFIER_LEN: usize = 64;

/// True for a template-safe identifier: `[A-Za-z_][A-Za-z0-9_]{0,63}`.
///
/// Input names and `output_var`s become template keys, so one containing `.`,
/// `{` or `}` would make a rendered payload unparseable — a failure that would
/// surface as a confusing JSON error inside an unrelated step.
pub fn valid_template_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_IDENTIFIER_LEN
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
    /// Parameters this procedure accepts at run time. Empty for a prose SOP.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ProcedureInput>,
    /// When this procedure was first created.
    pub created_at: DateTime<Utc>,
    /// When this procedure was last modified.
    pub updated_at: DateTime<Utc>,
    /// When this procedure was last injected into an agent context (None = never).
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    /// Times this procedure was injected into an agent context.
    #[serde(default)]
    pub use_count: u32,
    /// Lifecycle confidence (0..1), reinforced on use and decayed over time.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// Lifecycle status; non-active entries are excluded from default search.
    #[serde(default)]
    pub status: MemoryStatus,
}

impl Procedure {
    /// True when every step is a real tool call.
    ///
    /// All-or-nothing on purpose: a procedure with one prose step among four
    /// tool calls is not three-quarters executable. Running it would silently
    /// skip the prose step and produce a result that looks complete and is not,
    /// so `procedure-run` refuses it and names the step to fix.
    pub fn is_executable(&self) -> bool {
        !self.steps.is_empty()
            && self
                .steps
                .iter()
                .all(|step| step.tool.is_some() && step.input.is_some())
    }

    /// The first step that keeps this procedure from being executable.
    pub fn first_prose_step(&self) -> Option<&ProcedureStep> {
        self.steps
            .iter()
            .find(|step| step.tool.is_none() || step.input.is_none())
    }
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
