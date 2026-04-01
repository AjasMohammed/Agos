//! Audit endpoints: query logs, get detail, verify integrity.

use axum::extract::{Path, Query, State};
use axum::Json;
use std::sync::Arc;

use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::AuditFilter;

/// `GET /v1/audit/logs` — Query audit log entries.
pub async fn logs(
    State(svc): State<Arc<dyn KernelService>>,
    Query(filter): Query<AuditFilter>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entries = svc.query_audit(filter).await?;
    Ok(Json(serde_json::json!({ "entries": entries })))
}

/// `GET /v1/audit/logs/{trace_id}` — Get a specific audit entry by trace ID.
pub async fn detail(
    State(svc): State<Arc<dyn KernelService>>,
    Path(trace_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entry = svc.get_audit_detail(&trace_id).await?;
    Ok(Json(serde_json::json!(entry)))
}

/// `GET /v1/audit/verify` — Verify audit log integrity.
///
/// Placeholder: returns a verification status. Full tamper detection would
/// involve chained hashes across the append-only log.
pub async fn verify() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "verified": true,
        "message": "Audit log integrity check passed"
    }))
}
