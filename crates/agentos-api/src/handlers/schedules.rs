//! Schedule (cron automation) endpoints: list, create, preview, pause, resume,
//! delete, runs.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::str::FromStr;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{
    ApiScheduleRun, ApiScheduleSummary, CreateScheduleRequest, CronPreviewRequest,
    CronPreviewResponse,
};

/// `GET /api/v1/schedules` — List all scheduled entries: recurring cron
/// schedules plus agent-created one-shot once-jobs and timers (see `kind`).
#[utoipa::path(
    get,
    path = "/api/v1/schedules",
    tag = "schedules",
    operation_id = "schedules_list",
    responses(
        (status = 200, description = "List of schedules", body = crate::response::Envelope<Vec<crate::types::ApiScheduleSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiScheduleSummary>>>, ApiError> {
    require_permission(&key, "schedules:r")?;
    let schedules = svc.list_schedules().await?;
    Ok(Json(Envelope::new(schedules)))
}

/// `POST /api/v1/schedules` — Create a new cron schedule.
#[utoipa::path(
    post,
    path = "/api/v1/schedules",
    tag = "schedules",
    operation_id = "schedules_create",
    request_body = CreateScheduleRequest,
    responses(
        (status = 200, description = "Schedule created", body = crate::response::Envelope<crate::types::ApiScheduleSummary>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<CreateScheduleRequest>,
) -> Result<Json<Envelope<ApiScheduleSummary>>, ApiError> {
    require_permission(&key, "schedules:w")?;
    let created = svc.create_schedule(req).await?;
    Ok(Json(Envelope::new(created)))
}

/// `POST /api/v1/schedules/preview` — Compute upcoming fire times for a cron
/// expression without creating anything. Pure; no kernel state is touched.
#[utoipa::path(
    post,
    path = "/api/v1/schedules/preview",
    tag = "schedules",
    operation_id = "schedules_preview",
    request_body = CronPreviewRequest,
    responses(
        (status = 200, description = "Upcoming fire times", body = crate::response::Envelope<crate::types::CronPreviewResponse>),
        (status = 400, description = "Invalid cron expression", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn preview(
    State(_svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<CronPreviewRequest>,
) -> Result<Json<Envelope<CronPreviewResponse>>, ApiError> {
    require_permission(&key, "schedules:r")?;
    let count = req.count.unwrap_or(5).clamp(1, 50);

    // Normalize 5-field cron to 6-field (prepend seconds), matching the
    // kernel's create_job behaviour, so preview agrees with creation.
    let expr = if req.cron.split_whitespace().count() == 5 {
        format!("0 {}", req.cron.trim())
    } else {
        req.cron.trim().to_string()
    };

    let schedule = cron::Schedule::from_str(&expr)
        .map_err(|e| ApiError::BadRequest(format!("Invalid cron expression: {e}")))?;
    let next_runs: Vec<String> = schedule
        .upcoming(chrono::Utc)
        .take(count)
        .map(|dt| dt.to_rfc3339())
        .collect();
    Ok(Json(Envelope::new(CronPreviewResponse { next_runs })))
}

/// `POST /api/v1/schedules/{id}/pause` — Pause a schedule.
#[utoipa::path(
    post,
    path = "/api/v1/schedules/{id}/pause",
    tag = "schedules",
    operation_id = "schedules_pause",
    params(("id" = String, Path, description = "Schedule ID")),
    responses(
        (status = 200, description = "Schedule paused", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Schedule not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn pause(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "schedules:w")?;
    svc.pause_schedule(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "paused": id }))))
}

/// `POST /api/v1/schedules/{id}/resume` — Resume a paused schedule.
#[utoipa::path(
    post,
    path = "/api/v1/schedules/{id}/resume",
    tag = "schedules",
    operation_id = "schedules_resume",
    params(("id" = String, Path, description = "Schedule ID")),
    responses(
        (status = 200, description = "Schedule resumed", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Schedule not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn resume(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "schedules:w")?;
    svc.resume_schedule(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "resumed": id }))))
}

/// `DELETE /api/v1/schedules/{id}` — Delete a schedule.
#[utoipa::path(
    delete,
    path = "/api/v1/schedules/{id}",
    tag = "schedules",
    operation_id = "schedules_delete",
    params(("id" = String, Path, description = "Schedule ID")),
    responses(
        (status = 200, description = "Schedule deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Schedule not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "schedules:w")?;
    svc.delete_schedule(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": id }))))
}

/// Query params for `GET /api/v1/schedules/{id}/runs`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ScheduleRunsQuery {
    /// Max number of runs to return (default 50, capped at 500).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `GET /api/v1/schedules/{id}/runs` — List recorded fires of a schedule.
#[utoipa::path(
    get,
    path = "/api/v1/schedules/{id}/runs",
    tag = "schedules",
    operation_id = "schedules_runs",
    params(
        ("id" = String, Path, description = "Schedule ID"),
        ScheduleRunsQuery
    ),
    responses(
        (status = 200, description = "Run history", body = crate::response::Envelope<Vec<crate::types::ApiScheduleRun>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Schedule not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn runs(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Query(q): Query<ScheduleRunsQuery>,
) -> Result<Json<Envelope<Vec<ApiScheduleRun>>>, ApiError> {
    require_permission(&key, "schedules:r")?;
    let limit = q.limit.unwrap_or(50).min(500);
    let runs = svc.get_schedule_runs(&id, limit).await?;
    Ok(Json(Envelope::new(runs)))
}
