//! System endpoints: health check, status.

use axum::extract::State;
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;

/// `GET /api/v1/health` — Public health check (no auth required).
#[utoipa::path(
    get,
    path = "/api/v1/health",
    tag = "system",
    operation_id = "system_health",
    responses(
        (status = 200, description = "Service healthy", body = serde_json::Value)
    )
)]
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "agentos-api",
    }))
}

/// `GET /api/v1/status` — System status with agent/task/tool counts.
#[utoipa::path(
    get,
    path = "/api/v1/status",
    tag = "system",
    operation_id = "system_status",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "System status", body = crate::response::Envelope<crate::types::SystemStatus>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn status(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<crate::types::SystemStatus>>, ApiError> {
    require_permission(&key, "system:r")?;
    let s = svc.get_status().await?;
    Ok(Json(Envelope::new(s)))
}
