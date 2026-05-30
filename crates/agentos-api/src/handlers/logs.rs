//! Log query endpoint: filter the append-only audit JSONL by level/time.

use axum::extract::{Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{LogLine, LogQuery};

/// `GET /api/v1/logs` — Query the audit log with optional level/since filters.
#[utoipa::path(
    get,
    path = "/api/v1/logs",
    tag = "system",
    operation_id = "logs_query",
    params(LogQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Matching log lines", body = crate::response::Envelope<Vec<crate::types::LogLine>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    )
)]
pub async fn query(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(q): Query<LogQuery>,
) -> Result<Json<Envelope<Vec<LogLine>>>, ApiError> {
    require_permission(&key, "system:r")?;
    let limit = q.limit.unwrap_or(200);
    let lines = svc.query_logs(q.level, q.since, limit).await?;
    Ok(Json(Envelope::new(lines)))
}
