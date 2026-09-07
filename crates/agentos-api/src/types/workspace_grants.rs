//! DTOs for the workspace-grant (folder access) REST surface.
//!
//! A workspace grant records that one agent — or every agent, when `agent_name`
//! is omitted — may read/write/exec inside a host directory tree. Without a
//! grant, file tools targeting an absolute path outside `data_dir` return
//! `PermissionDenied`. Backed by the kernel's `WorkspaceGrantStore`; the same
//! store the `agentos workspace` CLI writes to.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An active grant authorising filesystem access to one or all agents.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiWorkspaceGrant {
    pub id: i64,
    /// Absolute, lexically-normalized host directory. Subpaths are covered.
    pub path: String,
    /// Agent UUID this grant is scoped to; `null` means every agent.
    pub agent_id: Option<String>,
    /// Permission bits as a short string: any of `r`, `w`, `x` (e.g. `"rw"`).
    pub mode: String,
    pub granted_at: DateTime<Utc>,
    /// Where the grant came from (`config`, `bus`, `api`).
    pub source: String,
    pub granted_by: String,
}

/// Request body for `POST /api/v1/workspace-grants`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GrantWorkspaceRequest {
    /// Absolute host path. `~` is NOT expanded — the caller is a browser, not
    /// a shell, so it has no home directory to expand against.
    pub path: String,
    /// Permission bits: any combination of `r`, `w`, `x`. Defaults to `rw`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Agent display name or `AgentID` UUID. Omit for a global grant.
    #[serde(default)]
    pub agent_name: Option<String>,
}

/// Query parameters for `DELETE /api/v1/workspace-grants`.
///
/// Revocation matches on `(path, agent scope)`, not on the row id, so an
/// agent-scoped grant is only revoked when `agent_name` matches the original
/// scope (omit it for a global grant).
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
pub struct RevokeWorkspaceQuery {
    pub path: String,
    #[serde(default)]
    pub agent_name: Option<String>,
}

/// Query parameters for `GET /api/v1/workspace-grants`.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
pub struct ListWorkspaceGrantsQuery {
    /// Agent display name or UUID; returns that agent's grants plus the global
    /// ones. Omit to list every active grant.
    #[serde(default)]
    pub agent_name: Option<String>,
}
