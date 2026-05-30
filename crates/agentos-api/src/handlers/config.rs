//! Configuration endpoints: read the (redacted) config tree, read a dotted
//! key, and write a dotted key (gated by `[api] config_writable`).

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ConfigTree, ConfigValue, SetConfigRequest};

/// `GET /api/v1/config` — Full config tree with secret-bearing leaves redacted.
#[utoipa::path(
    get,
    path = "/api/v1/config",
    tag = "config",
    operation_id = "config_get_tree",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Redacted config tree", body = crate::response::Envelope<crate::types::ConfigTree>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn get_tree(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<ConfigTree>>, ApiError> {
    require_permission(&key, "system:r")?;
    let config = svc.get_config_tree().await?;
    Ok(Json(Envelope::new(ConfigTree { config })))
}

/// `GET /api/v1/config/{key}` — Resolve a dotted config key from the live file.
#[utoipa::path(
    get,
    path = "/api/v1/config/{key}",
    tag = "config",
    operation_id = "config_get_key",
    params(("key" = String, Path, description = "Dotted config key, e.g. llm.primary")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Resolved config value", body = crate::response::Envelope<crate::types::ConfigValue>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Key not found", body = crate::error::ApiErrorBody)
    )
)]
pub async fn get_key(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(dotted): Path<String>,
) -> Result<Json<Envelope<ConfigValue>>, ApiError> {
    require_permission(&key, "system:r")?;
    let value = svc.get_config_key(&dotted).await?;
    Ok(Json(Envelope::new(ConfigValue { key: dotted, value })))
}

/// `PUT /api/v1/config/{key}` — Write a dotted config key (gated by
/// `[api] config_writable`; returns 403 when disabled). The running kernel's
/// `ConfigWatcher` auto-reloads after the write.
#[utoipa::path(
    put,
    path = "/api/v1/config/{key}",
    tag = "config",
    operation_id = "config_set_key",
    params(("key" = String, Path, description = "Dotted config key to set")),
    request_body = SetConfigRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Value written", body = crate::response::Envelope<crate::types::ConfigValue>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 403, description = "Config writes disabled or insufficient scope", body = crate::error::ApiErrorBody)
    )
)]
pub async fn set_key(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(dotted): Path<String>,
    Json(req): Json<SetConfigRequest>,
) -> Result<Json<Envelope<ConfigValue>>, ApiError> {
    require_permission(&key, "config:w")?;
    if !svc.config_writable() {
        return Err(ApiError::Forbidden(
            "Config writes are disabled (set [api] config_writable = true to enable)".to_string(),
        ));
    }
    svc.set_config_key(&dotted, req.value.clone()).await?;
    Ok(Json(Envelope::new(ConfigValue {
        key: dotted,
        value: req.value,
    })))
}
