//! Dashboard endpoint: composite home-screen summary.

use axum::extract::State;
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;

/// `GET /api/v1/dashboard` — Composite dashboard summary (agents, task counts,
/// tool count, uptime, recent audit, background tasks).
#[utoipa::path(
    get,
    path = "/api/v1/dashboard",
    tag = "system",
    operation_id = "dashboard_get",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Dashboard summary", body = crate::response::Envelope<crate::types::DashboardSummary>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<crate::types::DashboardSummary>>, ApiError> {
    require_permission(&key, "system:r")?;
    let summary = svc.get_dashboard_summary().await?;
    Ok(Json(Envelope::new(summary)))
}
