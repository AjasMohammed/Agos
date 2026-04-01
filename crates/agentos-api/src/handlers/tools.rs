//! Tool endpoints: list, install, remove.

use axum::extract::{Path, State};
use axum::Json;
use std::sync::Arc;

use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::InstallToolRequest;

/// `GET /v1/tools` — List all registered tools.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tools = svc.list_tools().await?;
    Ok(Json(serde_json::json!({ "tools": tools })))
}

/// `GET /v1/tools/{name}` — Get a specific tool by name.
///
/// Note: The `KernelService` trait does not have a `get_tool` method, so we
/// filter the full list. This is acceptable for the current tool count.
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tools = svc.list_tools().await?;
    let tool = tools
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| ApiError::NotFound(format!("Tool '{name}' not found")))?;
    Ok(Json(serde_json::json!(tool)))
}

/// `POST /v1/tools` — Install a tool from a manifest path.
pub async fn install(
    State(svc): State<Arc<dyn KernelService>>,
    Json(req): Json<InstallToolRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tool_id = svc.install_tool(req).await?;
    Ok(Json(serde_json::json!({ "tool_id": tool_id.to_string() })))
}

/// `DELETE /v1/tools/{name}` — Remove a tool by name.
pub async fn remove(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    svc.remove_tool(&name).await?;
    Ok(Json(serde_json::json!({ "removed": name })))
}
