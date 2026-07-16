//! Escalation endpoints: list pending/all, get one, resolve (human-in-the-loop).

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{
    ApiEscalation, EscalationListQuery, ResolveEscalationRequest, ResolveEscalationResponse,
};

/// `GET /api/v1/escalations` — List escalations (pending by default, or all).
#[utoipa::path(
    get,
    path = "/api/v1/escalations",
    tag = "escalations",
    operation_id = "escalations_list",
    params(crate::types::EscalationListQuery),
    responses(
        (status = 200, description = "List of escalations", body = crate::response::Envelope<Vec<crate::types::ApiEscalation>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(query): Query<EscalationListQuery>,
) -> Result<Json<Envelope<Vec<ApiEscalation>>>, ApiError> {
    require_permission(&key, "escalations:r")?;
    let pending_only = query.pending.unwrap_or(true);
    let escalations = svc.list_escalations(pending_only).await?;
    Ok(Json(Envelope::new(escalations)))
}

/// `GET /api/v1/escalations/{id}` — Get a single escalation by numeric ID.
#[utoipa::path(
    get,
    path = "/api/v1/escalations/{id}",
    tag = "escalations",
    operation_id = "escalations_get",
    params(("id" = u64, Path, description = "Escalation ID")),
    responses(
        (status = 200, description = "Escalation detail", body = crate::response::Envelope<crate::types::ApiEscalation>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Escalation not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<u64>,
) -> Result<Json<Envelope<ApiEscalation>>, ApiError> {
    require_permission(&key, "escalations:r")?;
    let escalation = svc.get_escalation(id).await?;
    Ok(Json(Envelope::new(escalation)))
}

/// `POST /api/v1/escalations/{id}/resolve` — Resolve an escalation with a decision.
///
/// Returns 409 Conflict if the escalation is already resolved or expired.
#[utoipa::path(
    post,
    path = "/api/v1/escalations/{id}/resolve",
    tag = "escalations",
    operation_id = "escalations_resolve",
    params(("id" = u64, Path, description = "Escalation ID")),
    request_body = ResolveEscalationRequest,
    responses(
        (status = 200, description = "Escalation resolved", body = crate::response::Envelope<crate::types::ResolveEscalationResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Escalation not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Escalation already resolved or expired", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn resolve(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<u64>,
    Json(req): Json<ResolveEscalationRequest>,
) -> Result<Json<Envelope<ResolveEscalationResponse>>, ApiError> {
    require_permission(&key, "escalations:w")?;
    let resp = svc.resolve_escalation(id, req.decision, req.note).await?;
    Ok(Json(Envelope::new(resp)))
}
