//! Tool endpoints: list, install, remove.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::InstallToolRequest;

/// `GET /api/v1/tools` — List all registered tools.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "tools:r")?;
    let tools = svc.list_tools().await?;
    Ok(Json(serde_json::json!({ "data": tools })))
}

/// `GET /api/v1/tools/{name}` — Get a specific tool by name.
///
/// Note: The `KernelService` trait does not have a `get_tool` method, so we
/// filter the full list. This is acceptable for the current tool count.
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "tools:r")?;
    let tools = svc.list_tools().await?;
    let tool = tools
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| ApiError::NotFound(format!("Tool '{name}' not found")))?;
    Ok(Json(serde_json::json!({ "data": tool })))
}

/// `POST /api/v1/tools` — Install a tool from a manifest path.
pub async fn install(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<InstallToolRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "tools:w")?;
    // I11: Path traversal validation
    if req.manifest_path.contains("..") {
        return Err(ApiError::BadRequest(
            "Path traversal ('..') not allowed in manifest_path".into(),
        ));
    }
    let tool_id = svc.install_tool(req).await?;
    Ok(Json(
        serde_json::json!({ "data": { "tool_id": tool_id.to_string() } }),
    ))
}

/// `DELETE /api/v1/tools/{name}` — Remove a tool by name.
pub async fn remove(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "tools:w")?;
    svc.remove_tool(&name).await?;
    Ok(Json(serde_json::json!({ "data": { "removed": name } })))
}
