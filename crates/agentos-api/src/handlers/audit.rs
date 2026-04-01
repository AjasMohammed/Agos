//! Audit endpoints: query logs, get detail, verify integrity.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::AuditFilter;

/// `GET /api/v1/audit/logs` — Query audit log entries.
pub async fn logs(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(filter): Query<AuditFilter>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "audit:r")?;
    let entries = svc.query_audit(filter).await?;
    Ok(Json(serde_json::json!({ "data": entries })))
}

/// `GET /api/v1/audit/logs/{trace_id}` — Get a specific audit entry by trace ID.
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(trace_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "audit:r")?;
    let entry = svc.get_audit_detail(&trace_id).await?;
    Ok(Json(serde_json::json!({ "data": entry })))
}

/// `GET /api/v1/audit/verify` — Verify audit log integrity.
///
/// Returns 501 Not Implemented until full tamper detection (chained hashes
/// across the append-only log) is wired.
pub async fn verify(
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "audit:r")?;
    Err(ApiError::NotImplemented(
        "Audit chain verification not yet wired".into(),
    ))
}
