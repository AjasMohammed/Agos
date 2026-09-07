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

/// `POST /api/v1/mcp` — Attach an MCP server at runtime (stdio or http).
#[utoipa::path(
    post,
    path = "/api/v1/mcp",
    tag = "mcp",
    operation_id = "mcp_attach",
    security(("bearer_auth" = [])),
    request_body = crate::types::AttachMcpRequest,
    responses(
        (status = 200, description = "Server attached; lists registered tool names", body = crate::response::Envelope<crate::types::McpAttachedResponse>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 409, description = "Attach failed (duplicate name, handshake, vault)", body = crate::error::ApiErrorBody)
    )
)]
pub async fn attach(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<crate::types::AttachMcpRequest>,
) -> Result<Json<Envelope<crate::types::McpAttachedResponse>>, ApiError> {
    require_permission(&key, "mcp:w")?;
    Ok(Json(Envelope::new(svc.attach_mcp_server(req).await?)))
}

/// `GET /api/v1/mcp/catalog?q=` — Browse the curated MCP catalog.
#[utoipa::path(
    get,
    path = "/api/v1/mcp/catalog",
    tag = "mcp",
    operation_id = "mcp_catalog_list",
    security(("bearer_auth" = [])),
    params(crate::types::McpCatalogQuery),
    responses(
        (status = 200, description = "Catalog entries", body = crate::response::Envelope<Vec<crate::types::ApiMcpCatalogEntry>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn catalog_list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    axum::extract::Query(q): axum::extract::Query<crate::types::McpCatalogQuery>,
) -> Result<Json<Envelope<Vec<crate::types::ApiMcpCatalogEntry>>>, ApiError> {
    require_permission(&key, "mcp:r")?;
    Ok(Json(Envelope::new(
        svc.list_mcp_catalog(q.q.as_deref()).await?,
    )))
}

/// `GET /api/v1/mcp/catalog/{id}` — Full catalog entry.
#[utoipa::path(
    get,
    path = "/api/v1/mcp/catalog/{id}",
    tag = "mcp",
    operation_id = "mcp_catalog_detail",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Catalog entry id")),
    responses(
        (status = 200, description = "Catalog entry", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn catalog_detail(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "mcp:r")?;
    Ok(Json(Envelope::new(svc.get_mcp_catalog_entry(&id).await?)))
}

/// `POST /api/v1/mcp/catalog/{id}/install` — Install a catalog entry (attaches under its id).
#[utoipa::path(
    post,
    path = "/api/v1/mcp/catalog/{id}/install",
    tag = "mcp",
    operation_id = "mcp_catalog_install",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Catalog entry id")),
    request_body = crate::types::InstallMcpRequest,
    responses(
        (status = 200, description = "Installed", body = crate::response::Envelope<crate::types::McpAttachedResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Install refused (trust tier, runtime, attach)", body = crate::error::ApiErrorBody)
    )
)]
pub async fn catalog_install(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    body: Option<Json<crate::types::InstallMcpRequest>>,
) -> Result<Json<Envelope<crate::types::McpAttachedResponse>>, ApiError> {
    require_permission(&key, "mcp:w")?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    Ok(Json(Envelope::new(svc.install_mcp_server(&id, req).await?)))
}

/// `PUT /api/v1/mcp/{name}` — Re-attach a server with a new configuration.
#[utoipa::path(
    put,
    path = "/api/v1/mcp/{name}",
    tag = "mcp",
    operation_id = "mcp_update",
    security(("bearer_auth" = [])),
    params(("name" = String, Path, description = "MCP server name")),
    request_body = crate::types::AttachMcpRequest,
    responses(
        (status = 200, description = "Server re-attached", body = crate::response::Envelope<crate::types::McpAttachedResponse>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Server not attached", body = crate::error::ApiErrorBody),
        (status = 409, description = "New configuration failed to attach", body = crate::error::ApiErrorBody)
    )
)]
pub async fn update(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(req): Json<crate::types::AttachMcpRequest>,
) -> Result<Json<Envelope<crate::types::McpAttachedResponse>>, ApiError> {
    require_permission(&key, "mcp:w")?;
    Ok(Json(Envelope::new(
        svc.update_mcp_server(&name, req).await?,
    )))
}
