//! MCP endpoints: list servers, detach.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::ApiMcpServer;

/// `GET /api/v1/mcp` — List MCP servers (live + persisted attachments).
#[utoipa::path(
    get,
    path = "/api/v1/mcp",
    tag = "mcp",
    operation_id = "mcp_list",
    responses(
        (status = 200, description = "List of MCP servers", body = crate::response::Envelope<Vec<ApiMcpServer>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiMcpServer>>>, ApiError> {
    require_permission(&key, "mcp:r")?;
    Ok(Json(Envelope::new(svc.list_mcp_servers().await?)))
}

/// `POST /api/v1/mcp/{name}/detach` — Stop and remove an MCP server.
#[utoipa::path(
    post,
    path = "/api/v1/mcp/{name}/detach",
    tag = "mcp",
    operation_id = "mcp_detach",
    params(("name" = String, Path, description = "MCP server name")),
    responses(
        (status = 200, description = "Server detached", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Server not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn detach(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "mcp:w")?;
    svc.detach_mcp_server(&name).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "detached": name }))))
}
