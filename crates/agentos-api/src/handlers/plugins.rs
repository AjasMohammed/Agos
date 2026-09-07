//! Plugin endpoints: list, discover, detail, enable, disable.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiPluginDetail, ApiPluginSummary, DiscoverPluginsResponse};

/// `GET /api/v1/plugins` — List discovered plugins.
#[utoipa::path(
    get,
    path = "/api/v1/plugins",
    tag = "plugins",
    operation_id = "plugins_list",
    responses(
        (status = 200, description = "List of plugins", body = crate::response::Envelope<Vec<ApiPluginSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiPluginSummary>>>, ApiError> {
    require_permission(&key, "plugins:r")?;
    Ok(Json(Envelope::new(svc.list_plugins().await?)))
}

/// `POST /api/v1/plugins/discover` — Re-scan plugin directories.
#[utoipa::path(
    post,
    path = "/api/v1/plugins/discover",
    tag = "plugins",
    operation_id = "plugins_discover",
    responses(
        (status = 200, description = "Discovery result", body = crate::response::Envelope<DiscoverPluginsResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn discover(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<DiscoverPluginsResponse>>, ApiError> {
    require_permission(&key, "plugins:w")?;
    Ok(Json(Envelope::new(svc.discover_plugins().await?)))
}

/// `GET /api/v1/plugins/{id}` — Plugin detail.
#[utoipa::path(
    get,
    path = "/api/v1/plugins/{id}",
    tag = "plugins",
    operation_id = "plugins_detail",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Plugin detail", body = crate::response::Envelope<ApiPluginDetail>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Plugin not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<ApiPluginDetail>>, ApiError> {
    require_permission(&key, "plugins:r")?;
    Ok(Json(Envelope::new(svc.get_plugin(&id).await?)))
}

/// `POST /api/v1/plugins/{id}/enable` — Activate a plugin.
#[utoipa::path(
    post,
    path = "/api/v1/plugins/{id}/enable",
    tag = "plugins",
    operation_id = "plugins_enable",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Plugin enabled", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 409, description = "Plugin blocked", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn enable(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "plugins:w")?;
    svc.set_plugin_enabled(&id, true).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "enabled": id }))))
}

/// `POST /api/v1/plugins/{id}/disable` — Deactivate a plugin.
#[utoipa::path(
    post,
    path = "/api/v1/plugins/{id}/disable",
    tag = "plugins",
    operation_id = "plugins_disable",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Plugin disabled", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 409, description = "Plugin blocked", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn disable(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "plugins:w")?;
    svc.set_plugin_enabled(&id, false).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "disabled": id }))))
}

/// `POST /api/v1/plugins` — Install a plugin from a pasted `plugin.toml`.
#[utoipa::path(
    post,
    path = "/api/v1/plugins",
    tag = "plugins",
    operation_id = "plugins_install",
    security(("bearer_auth" = [])),
    request_body = crate::types::InstallPluginRequest,
    responses(
        (status = 200, description = "Installed (status may be `blocked` if unsigned)", body = crate::response::Envelope<ApiPluginSummary>),
        (status = 400, description = "Invalid manifest", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 409, description = "Plugin id already exists", body = crate::error::ApiErrorBody)
    )
)]
pub async fn install(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<crate::types::InstallPluginRequest>,
) -> Result<Json<Envelope<ApiPluginSummary>>, ApiError> {
    require_permission(&key, "plugins:w")?;
    Ok(Json(Envelope::new(
        svc.install_plugin(&req.manifest_toml).await?,
    )))
}

/// `DELETE /api/v1/plugins/{id}` — Remove a user-installed plugin (core plugins refuse).
#[utoipa::path(
    delete,
    path = "/api/v1/plugins/{id}",
    tag = "plugins",
    operation_id = "plugins_remove",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Removed", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Not a user-installed plugin", body = crate::error::ApiErrorBody)
    )
)]
pub async fn remove(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "plugins:w")?;
    svc.remove_plugin(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "removed": id }))))
}

/// `PUT /api/v1/plugins/{id}` — Replace a user plugin's manifest in place.
#[utoipa::path(
    put,
    path = "/api/v1/plugins/{id}",
    tag = "plugins",
    operation_id = "plugins_update",
    security(("bearer_auth" = [])),
    params(("id" = String, Path, description = "Plugin id")),
    request_body = crate::types::InstallPluginRequest,
    responses(
        (status = 200, description = "Updated", body = crate::response::Envelope<crate::types::ApiPluginSummary>),
        (status = 400, description = "Invalid manifest or id mismatch", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Built-in plugin", body = crate::error::ApiErrorBody)
    )
)]
pub async fn update(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(req): Json<crate::types::InstallPluginRequest>,
) -> Result<Json<Envelope<crate::types::ApiPluginSummary>>, ApiError> {
    require_permission(&key, "plugins:w")?;
    Ok(Json(Envelope::new(
        svc.update_plugin(&id, &req.manifest_toml).await?,
    )))
}
