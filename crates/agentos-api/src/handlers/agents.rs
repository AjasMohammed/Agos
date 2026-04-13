//! Agent endpoints: list, connect, disconnect, detail, permissions.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::{ConnectAgentRequest, PermissionRequest, UpdateAgentSettingsRequest};

/// `GET /api/v1/agents` — List all connected agents.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "agents:r")?;
    let agents = svc.list_agents().await?;
    Ok(Json(serde_json::json!({ "data": agents })))
}

/// `POST /api/v1/agents` — Connect a new agent.
pub async fn connect(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<ConnectAgentRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "agents:w")?;
    let agent = svc.connect_agent(req).await?;
    Ok(Json(serde_json::json!({ "data": agent })))
}

/// `GET /api/v1/agents/{name}` — Get detailed info for a single agent.
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "agents:r")?;
    let detail = svc.get_agent_detail(&name).await?;
    Ok(Json(serde_json::json!({ "data": detail })))
}

/// `POST /api/v1/agents/{name}/settings` — Update editable settings for an agent.
pub async fn update_settings(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(mut req): Json<UpdateAgentSettingsRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "agents:w")?;
    req.agent_name = name;
    svc.update_agent_settings(req).await?;
    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

/// `DELETE /api/v1/agents/{name}` — Disconnect an agent by name.
///
/// We look up the agent ID from the name via `get_agent_detail`, then call
/// `disconnect_agent`.
pub async fn disconnect(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "agents:w")?;
    let detail = svc.get_agent_detail(&name).await?;
    svc.disconnect_agent(detail.summary.id).await?;
    Ok(Json(
        serde_json::json!({ "data": { "disconnected": name } }),
    ))
}

/// `POST /api/v1/agents/{name}/permissions` — Grant a permission to an agent.
pub async fn grant_permission(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(mut req): Json<PermissionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "agents:w")?;
    req.agent_name = name;
    svc.grant_permission(req).await?;
    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

/// `POST /api/v1/agents/{name}/permissions/revoke` — Revoke a permission.
pub async fn revoke_permission(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(mut req): Json<PermissionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "agents:w")?;
    req.agent_name = name;
    svc.revoke_permission(req).await?;
    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}
