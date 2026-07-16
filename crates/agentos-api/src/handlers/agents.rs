//! Agent endpoints: list, connect, disconnect, detail, permissions.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ConnectAgentRequest, PermissionRequest, UpdateAgentSettingsRequest};

/// `GET /api/v1/agents` — List all connected agents.
#[utoipa::path(
    get,
    path = "/api/v1/agents",
    tag = "agents",
    operation_id = "agents_list",
    responses(
        (status = 200, description = "List of agents", body = crate::response::Envelope<Vec<crate::types::ApiAgentSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<crate::types::ApiAgentSummary>>>, ApiError> {
    require_permission(&key, "agents:r")?;
    let agents = svc.list_agents().await?;
    Ok(Json(Envelope::new(agents)))
}

/// `POST /api/v1/agents` — Connect a new agent.
#[utoipa::path(
    post,
    path = "/api/v1/agents",
    tag = "agents",
    operation_id = "agents_connect",
    request_body = ConnectAgentRequest,
    responses(
        (status = 200, description = "Connected agent", body = crate::response::Envelope<crate::types::ApiAgentSummary>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn connect(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<ConnectAgentRequest>,
) -> Result<Json<Envelope<crate::types::ApiAgentSummary>>, ApiError> {
    require_permission(&key, "agents:w")?;
    let agent = svc.connect_agent(req).await?;
    Ok(Json(Envelope::new(agent)))
}

/// `GET /api/v1/agents/{name}` — Get detailed info for a single agent.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{name}",
    tag = "agents",
    operation_id = "agents_detail",
    params(("name" = String, Path, description = "Agent name")),
    responses(
        (status = 200, description = "Agent detail", body = crate::response::Envelope<crate::types::ApiAgentDetail>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<crate::types::ApiAgentDetail>>, ApiError> {
    require_permission(&key, "agents:r")?;
    let detail = svc.get_agent_detail(&name).await?;
    Ok(Json(Envelope::new(detail)))
}

/// `POST /api/v1/agents/{name}/settings` — Update editable settings for an agent.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{name}/settings",
    tag = "agents",
    operation_id = "agents_update_settings",
    params(("name" = String, Path, description = "Agent name")),
    request_body = UpdateAgentSettingsRequest,
    responses(
        (status = 200, description = "Settings updated", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_settings(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(mut req): Json<UpdateAgentSettingsRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "agents:w")?;
    req.agent_name = name;
    svc.update_agent_settings(req).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "ok": true }))))
}

/// `DELETE /api/v1/agents/{name}` — Disconnect an agent by name.
///
/// We look up the agent ID from the name via `get_agent_detail`, then call
/// `disconnect_agent`.
#[utoipa::path(
    delete,
    path = "/api/v1/agents/{name}",
    tag = "agents",
    operation_id = "agents_disconnect",
    params(("name" = String, Path, description = "Agent name")),
    responses(
        (status = 200, description = "Agent disconnected", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn disconnect(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "agents:w")?;
    let detail = svc.get_agent_detail(&name).await?;
    svc.disconnect_agent(detail.summary.id).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "disconnected": name }),
    )))
}

/// `POST /api/v1/agents/{name}/permissions` — Grant a permission to an agent.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{name}/permissions",
    tag = "agents",
    operation_id = "agents_grant_permission",
    params(("name" = String, Path, description = "Agent name")),
    request_body = PermissionRequest,
    responses(
        (status = 200, description = "Permission granted", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn grant_permission(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(mut req): Json<PermissionRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "agents:w")?;
    req.agent_name = name;
    svc.grant_permission(req).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "ok": true }))))
}

/// `POST /api/v1/agents/{name}/permissions/revoke` — Revoke a permission.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{name}/permissions/revoke",
    tag = "agents",
    operation_id = "agents_revoke_permission",
    params(("name" = String, Path, description = "Agent name")),
    request_body = PermissionRequest,
    responses(
        (status = 200, description = "Permission revoked", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_permission(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(mut req): Json<PermissionRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "agents:w")?;
    req.agent_name = name;
    svc.revoke_permission(req).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "ok": true }))))
}
