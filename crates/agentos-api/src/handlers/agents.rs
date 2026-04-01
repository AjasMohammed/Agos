//! Agent endpoints: list, connect, disconnect, detail, permissions.

use axum::extract::{Path, State};
use axum::Json;
use std::sync::Arc;

use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::{ConnectAgentRequest, PermissionRequest};

/// `GET /v1/agents` — List all connected agents.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let agents = svc.list_agents().await?;
    Ok(Json(serde_json::json!({ "agents": agents })))
}

/// `POST /v1/agents` — Connect a new agent.
pub async fn connect(
    State(svc): State<Arc<dyn KernelService>>,
    Json(req): Json<ConnectAgentRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let agent = svc.connect_agent(req).await?;
    Ok(Json(serde_json::json!({ "agent": agent })))
}

/// `GET /v1/agents/{name}` — Get detailed info for a single agent.
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let detail = svc.get_agent_detail(&name).await?;
    Ok(Json(serde_json::json!(detail)))
}

/// `DELETE /v1/agents/{name}` — Disconnect an agent by name.
///
/// We look up the agent ID from the name via `get_agent_detail`, then call
/// `disconnect_agent`.
pub async fn disconnect(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let detail = svc.get_agent_detail(&name).await?;
    svc.disconnect_agent(detail.summary.id).await?;
    Ok(Json(serde_json::json!({ "disconnected": name })))
}

/// `POST /v1/agents/{name}/permissions` — Grant a permission to an agent.
pub async fn grant_permission(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
    Json(mut req): Json<PermissionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    req.agent_name = name;
    svc.grant_permission(req).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /v1/agents/{name}/permissions/revoke` — Revoke a permission.
pub async fn revoke_permission(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
    Json(mut req): Json<PermissionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    req.agent_name = name;
    svc.revoke_permission(req).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
