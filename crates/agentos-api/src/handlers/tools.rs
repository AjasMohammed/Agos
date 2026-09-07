//! Tool endpoints: list, install, remove.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::InstallToolRequest;

/// `GET /api/v1/tools` — List all registered tools.
#[utoipa::path(
    get, path = "/api/v1/tools", tag = "tools", operation_id = "tools_list",
    responses(
        (status = 200, description = "Registered tools", body = crate::response::Envelope<Vec<crate::types::ApiToolSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<crate::types::ApiToolSummary>>>, ApiError> {
    require_permission(&key, "tools:r")?;
    let tools = svc.list_tools().await?;
    Ok(Json(Envelope::new(tools)))
}

/// `GET /api/v1/tools/{name}` — Get a specific tool by name.
///
/// Note: The `KernelService` trait does not have a `get_tool` method, so we
/// filter the full list. This is acceptable for the current tool count.
#[utoipa::path(
    get, path = "/api/v1/tools/{name}", tag = "tools", operation_id = "tools_get",
    params(("name" = String, Path, description = "Tool name")),
    responses(
        (status = 200, description = "Tool detail", body = crate::response::Envelope<crate::types::ApiToolSummary>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Tool not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<crate::types::ApiToolSummary>>, ApiError> {
    require_permission(&key, "tools:r")?;
    let tools = svc.list_tools().await?;
    let tool = tools
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| ApiError::NotFound(format!("Tool '{name}' not found")))?;
    Ok(Json(Envelope::new(tool)))
}

/// `POST /api/v1/tools` — Install a tool from a manifest path.
#[utoipa::path(
    post, path = "/api/v1/tools", tag = "tools", operation_id = "tools_install",
    request_body = crate::types::InstallToolRequest,
    responses(
        (status = 200, description = "Installed tool", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn install(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<InstallToolRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "tools:w")?;
    // I11: Path traversal validation
    if req.manifest_path.contains("..") {
        return Err(ApiError::BadRequest(
            "Path traversal ('..') not allowed in manifest_path".into(),
        ));
    }
    let tool_id = svc.install_tool(req).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "tool_id": tool_id.to_string() }),
    )))
}

/// `DELETE /api/v1/tools/{name}` — Remove a tool by name.
#[utoipa::path(
    delete, path = "/api/v1/tools/{name}", tag = "tools", operation_id = "tools_remove",
    params(("name" = String, Path, description = "Tool name")),
    responses(
        (status = 200, description = "Removed tool", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Tool not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "tools:w")?;
    svc.remove_tool(&name).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "removed": name }))))
}
