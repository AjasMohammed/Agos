//! System endpoints: health check, status.

use axum::extract::State;
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;

/// `GET /api/v1/health` — Public health check (no auth required).
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "agentos-api",
    }))
}

/// `GET /api/v1/status` — System status with agent/task/tool counts.
pub async fn status(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "system:r")?;
    let s = svc.get_status().await?;
    Ok(Json(serde_json::json!({ "data": s })))
}
