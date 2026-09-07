//! DTOs for the approval-policy (standing-grant) REST surface.
//!
//! A standing grant is a persisted "allow always" entry: for a tool (optionally
//! scoped to a payload `path` glob and/or one agent), the approval hook lifts
//! `Prompt → Allow` instead of escalating. Backed by the kernel's
//! `ApprovalPolicyStore`; `expires_at` gives a time-boxed grant.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A persisted "allow always" approval policy (standing grant).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiApprovalPolicy {
    pub id: i64,
    pub tool_name: String,
    pub path_glob: Option<String>,
    pub agent_id: Option<String>,
    pub granted_at: DateTime<Utc>,
    pub granted_by: String,
    pub source: String,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Request body for `POST /api/v1/approval-policies`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AddApprovalPolicyRequest {
    /// Tool this standing grant auto-approves (matched exactly).
    pub tool_name: String,
    /// Optional payload `path` glob to scope the grant (e.g. `"/tmp/**"`).
    pub path_glob: Option<String>,
    /// Optional agent UUID to scope to; omit to apply to every agent.
    pub agent_id: Option<String>,
    /// Optional expiry (RFC3339); omit for a permanent grant.
    pub expires_at: Option<DateTime<Utc>>,
}
