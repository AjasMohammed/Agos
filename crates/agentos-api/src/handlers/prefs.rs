//! User-preference proposal endpoints: list, accept, reject, stats.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiPrefProposal, ApiProposalStats, PrefProposalQuery};

/// `GET /api/v1/prefs/proposals` — List user-preference proposals by status.
#[utoipa::path(
    get,
    path = "/api/v1/prefs/proposals",
    tag = "prefs",
    operation_id = "prefs_list_proposals",
    params(crate::types::PrefProposalQuery),
    responses(
        (status = 200, description = "List of proposals", body = crate::response::Envelope<Vec<crate::types::ApiPrefProposal>>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_proposals(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(query): Query<PrefProposalQuery>,
) -> Result<Json<Envelope<Vec<ApiPrefProposal>>>, ApiError> {
    require_permission(&key, "prefs:r")?;
    let status = query.status.unwrap_or_else(|| "pending".to_string());
    let limit = query.limit.unwrap_or(50);
    let proposals = svc.list_pref_proposals(status, limit).await?;
    Ok(Json(Envelope::new(proposals)))
}

/// `POST /api/v1/prefs/proposals/{id}/accept` — Accept a proposal.
#[utoipa::path(
    post,
    path = "/api/v1/prefs/proposals/{id}/accept",
    tag = "prefs",
    operation_id = "prefs_accept",
    params(("id" = String, Path, description = "Proposal ID")),
    responses(
        (status = 200, description = "Proposal accepted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Proposal not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Proposal already reviewed", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn accept(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "prefs:w")?;
    svc.accept_pref_proposal(id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "accepted": true }))))
}

/// `POST /api/v1/prefs/proposals/{id}/reject` — Reject a proposal.
#[utoipa::path(
    post,
    path = "/api/v1/prefs/proposals/{id}/reject",
    tag = "prefs",
    operation_id = "prefs_reject",
    params(("id" = String, Path, description = "Proposal ID")),
    responses(
        (status = 200, description = "Proposal rejected", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Proposal not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Proposal already reviewed", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn reject(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "prefs:w")?;
    svc.reject_pref_proposal(id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "rejected": true }))))
}

/// `GET /api/v1/prefs/stats` — Aggregate proposal counts.
#[utoipa::path(
    get,
    path = "/api/v1/prefs/stats",
    tag = "prefs",
    operation_id = "prefs_stats",
    responses(
        (status = 200, description = "Proposal statistics", body = crate::response::Envelope<crate::types::ApiProposalStats>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn stats(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<ApiProposalStats>>, ApiError> {
    require_permission(&key, "prefs:r")?;
    let stats = svc.pref_proposal_stats().await?;
    Ok(Json(Envelope::new(stats)))
}
