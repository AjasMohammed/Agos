//! Doctor endpoints: run diagnostic checks and attempt auto-repair.

use axum::extract::State;
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{DoctorFixRequest, DoctorReport};

/// `GET /api/v1/doctor` — Run all diagnostic checks (read-only).
#[utoipa::path(
    get,
    path = "/api/v1/doctor",
    tag = "system",
    operation_id = "doctor_checks",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Diagnostic report", body = crate::response::Envelope<crate::types::DoctorReport>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn checks(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<DoctorReport>>, ApiError> {
    require_permission(&key, "system:r")?;
    let checks = svc.run_doctor().await?;
    let all_ok = !checks.iter().any(|c| c.status == "fail");
    Ok(Json(Envelope::new(DoctorReport { checks, all_ok })))
}

/// `POST /api/v1/doctor/fix` — Attempt to auto-repair failing checks, then
/// re-run all checks and return the updated report.
#[utoipa::path(
    post,
    path = "/api/v1/doctor/fix",
    tag = "system",
    operation_id = "doctor_fix",
    request_body = DoctorFixRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Post-fix diagnostic report", body = crate::response::Envelope<crate::types::DoctorReport>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 403, description = "Insufficient scope", body = crate::error::ApiErrorBody)
    )
)]
pub async fn fix(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<DoctorFixRequest>,
) -> Result<Json<Envelope<DoctorReport>>, ApiError> {
    require_permission(&key, "system:w")?;
    svc.apply_doctor_fix(&req.check).await?;
    let checks = svc.run_doctor().await?;
    let all_ok = !checks.iter().any(|c| c.status == "fail");
    Ok(Json(Envelope::new(DoctorReport { checks, all_ok })))
}
