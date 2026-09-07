//! Workspace-grant (folder access) endpoints: list, grant, revoke.
//!
//! These expose the kernel's `WorkspaceGrantStore` — the durable list of host
//! directories each agent (or every agent) may read/write/exec inside — over
//! REST, so folder access is manageable from the panel instead of only through
//! `agentos workspace` on the CLI.

use axum::extract::{Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{
    ApiWorkspaceGrant, GrantWorkspaceRequest, ListWorkspaceGrantsQuery, RevokeWorkspaceQuery,
};

/// `GET /api/v1/workspace-grants` — List active folder-access grants.
#[utoipa::path(
    get,
    path = "/api/v1/workspace-grants",
    tag = "workspace-grants",
    operation_id = "workspace_grants_list",
    params(ListWorkspaceGrantsQuery),
    responses(
        (status = 200, description = "Active workspace grants", body = crate::response::Envelope<Vec<crate::types::ApiWorkspaceGrant>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(q): Query<ListWorkspaceGrantsQuery>,
) -> Result<Json<Envelope<Vec<ApiWorkspaceGrant>>>, ApiError> {
    require_permission(&key, "workspace:r")?;
    Ok(Json(Envelope::new(
        svc.list_workspace_grants(q.agent_name).await?,
    )))
}

/// `POST /api/v1/workspace-grants` — Grant a host directory to one agent, or to
/// every agent when `agent_name` is omitted.
///
/// Returns 409 Conflict when an active grant already exists for the same
/// (path, agent scope), and 400 for a relative path or a system root.
#[utoipa::path(
    post,
    path = "/api/v1/workspace-grants",
    tag = "workspace-grants",
    operation_id = "workspace_grants_add",
    request_body = GrantWorkspaceRequest,
    responses(
        (status = 200, description = "Grant created", body = crate::response::Envelope<crate::types::ApiWorkspaceGrant>),
        (status = 400, description = "Invalid path or mode", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Grant already exists for this scope", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn add(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<GrantWorkspaceRequest>,
) -> Result<Json<Envelope<ApiWorkspaceGrant>>, ApiError> {
    require_permission(&key, "workspace:w")?;
    Ok(Json(Envelope::new(
        svc.grant_workspace(req, &key.0.name).await?,
    )))
}

/// `DELETE /api/v1/workspace-grants?path=…&agent_name=…` — Revoke a grant.
///
/// Matches on the (path, agent scope) pair rather than a row id, mirroring
/// `agentos workspace revoke`: omit `agent_name` to revoke the global grant.
#[utoipa::path(
    delete,
    path = "/api/v1/workspace-grants",
    tag = "workspace-grants",
    operation_id = "workspace_grants_revoke",
    params(RevokeWorkspaceQuery),
    responses(
        (status = 200, description = "Grant revoked", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "No active grant for this path and scope", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(q): Query<RevokeWorkspaceQuery>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "workspace:w")?;
    let count = svc
        .revoke_workspace(q.path.clone(), q.agent_name, &key.0.name)
        .await?;
    if count == 0 {
        // A no-op revoke is a stale row in the caller's list, not a success.
        return Err(ApiError::NotFound(format!(
            "No active workspace grant for {}",
            q.path
        )));
    }
    Ok(Json(Envelope::new(
        serde_json::json!({ "revoked": true, "count": count }),
    )))
}
