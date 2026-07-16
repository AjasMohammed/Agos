//! DTOs for the read-only agent inbox (agent-to-agent message timeline).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Query for `GET /api/v1/agents/{id}/inbox`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct InboxQuery {
    /// Max messages to return, newest window (default 50, capped 200).
    pub limit: Option<usize>,
}

/// One agent-to-agent message in the timeline.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiInboxMessage {
    pub id: String,
    /// Sender agent id (UUID).
    pub from: String,
    /// Rendered target: `direct:<uuid>`, `name:<n>`, `group:<id>`, or `broadcast`.
    pub to: String,
    /// Content kind: `text` | `structured` | `delegation` | `result`.
    pub kind: String,
    /// Human-readable content preview (truncated).
    pub preview: String,
    /// Id of the message this replies to, if any.
    pub reply_to: Option<String>,
    pub timestamp: DateTime<Utc>,
    /// Whether the message carried a sender signature (presence only — not
    /// re-verified here).
    pub signed: bool,
}
