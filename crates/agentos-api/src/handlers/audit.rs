//! Audit endpoints: query logs, get detail, verify integrity.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::AuditFilter;

/// `GET /api/v1/audit/logs` — Query audit log entries.
#[utoipa::path(
    get, path = "/api/v1/audit/logs", tag = "audit", operation_id = "audit_logs",
    params(crate::types::AuditFilter),
    responses(
        (status = 200, description = "Audit entries", body = crate::response::Envelope<Vec<crate::types::AuditEntrySummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn logs(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(filter): Query<AuditFilter>,
) -> Result<Json<Envelope<Vec<crate::types::AuditEntrySummary>>>, ApiError> {
    require_permission(&key, "audit:r")?;
    let entries = svc.query_audit(filter).await?;
    Ok(Json(Envelope::new(entries)))
}

/// `GET /api/v1/audit/logs/{trace_id}` — Get a specific audit entry by trace ID.
#[utoipa::path(
    get, path = "/api/v1/audit/logs/{trace_id}", tag = "audit", operation_id = "audit_detail",
    params(("trace_id" = String, Path, description = "Trace ID")),
    responses(
        (status = 200, description = "Audit entry detail", body = crate::response::Envelope<crate::types::AuditEntryDetail>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Audit entry not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(trace_id): Path<String>,
) -> Result<Json<Envelope<crate::types::AuditEntryDetail>>, ApiError> {
    require_permission(&key, "audit:r")?;
    let entry = svc.get_audit_detail(&trace_id).await?;
    Ok(Json(Envelope::new(entry)))
}

/// `GET /api/v1/audit/verify` — Verify audit log integrity.
///
/// Returns 501 Not Implemented until full tamper detection (chained hashes
/// across the append-only log) is wired.
#[utoipa::path(
    get, path = "/api/v1/audit/verify", tag = "audit", operation_id = "audit_verify",
    responses(
        (status = 200, description = "Audit chain verification result", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn verify(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "audit:r")?;
    let result = svc.verify_audit_chain().await?;
    Ok(Json(Envelope::new(result)))
}
