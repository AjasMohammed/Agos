//! System-introspection endpoints: host resources + HAL device inventory.

use axum::extract::State;
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{HalInfo, ResourceInfo};

/// `GET /api/v1/resources` — Host memory/disk snapshot plus live resource locks.
#[utoipa::path(
    get,
    path = "/api/v1/resources",
    tag = "system",
    operation_id = "system_resources",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Resource snapshot", body = crate::response::Envelope<crate::types::ResourceInfo>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn resources(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<ResourceInfo>>, ApiError> {
    require_permission(&key, "system:r")?;
    let info = svc.get_resources().await?;
    Ok(Json(Envelope::new(info)))
}

/// `GET /api/v1/hal` — Hardware abstraction layer device inventory + snapshot.
#[utoipa::path(
    get,
    path = "/api/v1/hal",
    tag = "system",
    operation_id = "system_hal",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "HAL device inventory", body = crate::response::Envelope<crate::types::HalInfo>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn hal(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<HalInfo>>, ApiError> {
    require_permission(&key, "system:r")?;
    let info = svc.get_hal_info().await?;
    Ok(Json(Envelope::new(info)))
}
