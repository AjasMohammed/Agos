//! DTOs for the user-preference proposal (governance) REST surface.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Query filter for `GET /api/v1/prefs/proposals`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PrefProposalQuery {
    /// Proposal status to filter by: `pending` (default), `accepted`, or `rejected`.
    pub status: Option<String>,
    /// Maximum number of proposals to return (default 50).
    pub limit: Option<u32>,
}

/// A single user-preference proposal awaiting (or having received) review.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiPrefProposal {
    pub id: String,
    pub task_id: String,
    pub agent_id: String,
    /// Proposal kind: `add`, `replace`, or `delete`.
    pub kind: String,
    pub content: String,
    pub confidence: f32,
    pub evidence: Vec<String>,
    /// Proposal status: `pending`, `accepted`, `rejected`, or `expired`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

/// Aggregate counts across proposal lifecycle states.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiProposalStats {
    /// Total proposals ever recorded (sum of all other counts).
    pub proposed: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub pending: u64,
    pub expired: u64,
}
