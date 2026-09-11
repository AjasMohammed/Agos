//! DTOs for the chat-session management REST surface (Phase 02 Conversational).
//!
//! These mirror `agentos_kernel::chat_store::{ChatSession, ChatMessage}` shapes.
//! Message *send*/streaming is intentionally out of scope here — this surface is
//! read/manage only (list, create, rename, fork, delete, export, messages).

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Summary row for a chat session (list view).
///
/// Maps from `ChatStore::list_sessions()` → `ChatSession`. `preview` comes from
/// `ChatSession::last_preview` (last user/assistant message snippet).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiChatSessionSummary {
    /// Session id (UUID).
    pub id: String,
    /// Agent the session is bound to.
    pub agent_name: String,
    /// Optional user-defined title.
    pub title: Option<String>,
    /// Last message preview, if any.
    pub preview: Option<String>,
    /// Total number of messages in the session.
    pub message_count: u64,
    /// RFC3339 last-updated timestamp.
    pub updated_at: String,
}

/// Request body for `POST /api/v1/chat/sessions`.
///
/// `first_message` is optional: omit it (or send blank) to open an empty
/// session, which is what a client that creates the session on the first send
/// wants — the send/stream endpoints persist the user turn themselves.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateChatSessionRequest {
    /// Agent to bind the new session to.
    pub agent_name: String,
    /// Optional initial title (currently informational — the store sets title to
    /// NULL on create; rename via PATCH to set it).
    #[serde(default)]
    pub title: Option<String>,
    /// Optional first user message. Missing or blank means an empty session.
    #[serde(default)]
    pub first_message: Option<String>,
}

/// A single chat message in a session timeline.
///
/// Maps from `ChatStore::get_messages()` → `ChatMessage`. Tool activity is
/// surfaced inline via the optional `tool_*` fields (the store joins
/// `chat_tool_calls` onto the owning message); user/assistant rows leave them
/// `None`. Note: per-message `tokens`/`cost`, multimodal `parts`, and the
/// reasoning `thinking` trace are not persisted in `chat_messages` and are
/// therefore intentionally absent from this DTO.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiChatMessage {
    /// `"user"` | `"assistant"` | `"tool"`.
    pub role: String,
    /// Message text (or `Tool call: <name>` for tool rows).
    pub content: String,
    /// RFC3339 message timestamp.
    pub timestamp: String,
    /// Tool name (populated when `role == "tool"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool intent type (e.g. `"query"`, `"execute"`) when `role == "tool"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_intent_type: Option<String>,
    /// Tool input payload JSON string when `role == "tool"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_payload_json: Option<String>,
    /// Tool result payload JSON string when `role == "tool"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result_json: Option<String>,
    /// Tool success flag when `role == "tool"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_success: Option<bool>,
    /// Tool execution duration in milliseconds when `role == "tool"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_duration_ms: Option<u64>,
}

/// Detailed session view: header fields plus the full message timeline.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiChatSessionDetail {
    /// Session id (UUID).
    pub id: String,
    /// Agent the session is bound to.
    pub agent_name: String,
    /// Optional user-defined title.
    pub title: Option<String>,
    /// The session's messages, oldest-first.
    pub messages: Vec<ApiChatMessage>,
}

/// Request body for `PATCH /api/v1/chat/sessions/{id}` (rename).
///
/// `title = null` clears the title.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RenameChatSessionRequest {
    /// New title, or `null` to clear it.
    #[serde(default)]
    pub title: Option<String>,
}

/// Request body for `POST /api/v1/chat/sessions/{id}/fork`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ForkChatSessionRequest {
    /// Optional title for the forked copy. When omitted, the store derives
    /// `"<source title> (fork)"`.
    #[serde(default)]
    pub title: Option<String>,
}

/// Response body for a successful fork.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ForkChatSessionResponse {
    /// Id of the newly created forked session.
    pub id: String,
}

/// Query parameters for `GET /api/v1/chat/sessions/{id}/export`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams)]
pub struct ExportQuery {
    /// `"json"` (default) or `"markdown"`.
    #[serde(default)]
    pub format: Option<String>,
}

/// Request body for sending a user message to a session (non-streaming).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SendChatMessageRequest {
    pub text: String,
    /// Comma-separated upload ids attached to this message (from
    /// `GET /api/v1/files`). Resolved into context parts — extracted text for
    /// documents, image parts for a vision-capable agent.
    #[serde(default)]
    pub file_ids: Option<String>,
}
