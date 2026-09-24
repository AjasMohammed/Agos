//! DTOs for the agent-conversation (multi-agent convo) REST surface.
//!
//! These mirror `agentos_kernel::convo_store::{AgentConvo, ConvoTurn}`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Summary row for a multi-agent conversation (list view).
///
/// Maps from `ConvoStore::list_convos()` → `AgentConvo`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiConvoSummary {
    /// Conversation id (UUID).
    pub id: String,
    /// Conversation topic / seed prompt.
    pub topic: String,
    /// Ordered participant agent names.
    pub participants: Vec<String>,
    /// `"running"` | `"complete"` | `"stopped"` | `"error"`.
    pub status: String,
    /// RFC3339 last-updated timestamp.
    pub updated_at: String,
    /// How the thread started: `"operator"` (created from the UI or API) or
    /// `"dm"` (opened by an agent's `agent-message`). Older rows read
    /// `"operator"` — the column's default.
    pub kind: String,
}

/// A single turn within a conversation.
///
/// Maps from `ConvoStore::get_turns()` → `ConvoTurn`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiConvoTurn {
    /// 1-based turn ordering within the conversation.
    pub turn_number: u32,
    /// Agent that produced this turn, or `@user` for an operator message.
    pub agent_name: String,
    /// Turn content.
    pub content: String,
    /// RFC3339 creation timestamp.
    pub created_at: String,
}

/// Detailed conversation view: header fields plus the ordered turn list.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiConvoDetail {
    /// Conversation id (UUID).
    pub id: String,
    /// Conversation topic / seed prompt.
    pub topic: String,
    /// Ordered participant agent names.
    pub participants: Vec<String>,
    /// `"running"` | `"complete"` | `"stopped"` | `"error"`.
    pub status: String,
    /// Turn ceiling the orchestration loop runs to (clamped 2..=50 at creation).
    pub max_turns: u32,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-updated timestamp.
    pub updated_at: String,
    /// The conversation's turns, ordered by `turn_number`.
    pub messages: Vec<ApiConvoTurn>,
}

/// Request body for creating + running a new multi-agent conversation.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateConvoRequest {
    pub topic: String,
    pub participants: Vec<String>,
    /// Number of turns to run (clamped to 2..=50; default 8).
    #[serde(default)]
    pub max_turns: Option<u32>,
}

/// Request body for continuing a finished conversation in place.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContinueConvoRequest {
    /// Additional agent turns to run (clamped to 1..=50; default 8).
    #[serde(default)]
    pub turns: Option<u32>,
}

/// Request body for posting an operator message into a conversation.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PostConvoMessageRequest {
    /// Message text (max 4000 chars).
    pub content: String,
}
