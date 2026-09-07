//! DTOs for the escalations (human-in-the-loop) REST surface.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Query filter for `GET /api/v1/escalations`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EscalationListQuery {
    /// When `true`, return only pending (unresolved) escalations. When `false`
    /// or omitted, return all escalations (including resolved).
    pub pending: Option<bool>,
}

/// A single escalation awaiting (or having received) a human decision.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiEscalation {
    pub id: u64,
    pub task_id: String,
    pub agent_id: String,
    /// Debug-rendered escalation reason.
    pub reason: String,
    pub context_summary: String,
    pub decision_point: String,
    pub options: Vec<String>,
    pub urgency: String,
    pub blocking: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub resolved: bool,
    pub resolution: Option<String>,
    pub metadata: serde_json::Value,
}

/// Request body for `POST /api/v1/escalations/{id}/resolve`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResolveEscalationRequest {
    /// Operator decision, e.g. `"approve"` / `"approved"` / `"deny"` / `"denied"`.
    pub decision: String,
    /// Optional free-form note recorded alongside the decision.
    pub note: Option<String>,
    /// On approve, also mint a standing grant for the escalated tool
    /// (agent-scoped, 7 days; parent-directory glob when the call had a
    /// `path`) so it stops re-prompting. Ignored on deny. Requires the
    /// `approvals:w` scope in addition to `escalations:w`. Refused for
    /// exec-capable tools with no path to scope by. Revocable via
    /// `DELETE /api/v1/approval-policies/{id}`.
    #[serde(default)]
    pub remember: bool,
}

/// Response for `POST /api/v1/escalations/{id}/resolve`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResolveEscalationResponse {
    pub status: String,
    pub escalation_id: u64,
    pub task_id: String,
    pub task_resumed: bool,
    /// Id of the standing grant minted by `remember: true`, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<i64>,
    /// Human-readable outcome of `remember: true` — what was remembered, or
    /// why nothing was (already remembered, exec tool with no path, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_note: Option<String>,
}
