//! Read-only agent inbox: the agent-to-agent message timeline.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiInboxMessage, InboxQuery};

/// `GET /api/v1/agents/{id}/inbox` — Agent-to-agent message history for an agent
/// (messages it sent, received directly, or received via broadcast), oldest first.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{id}/inbox",
    tag = "agents",
    operation_id = "agent_inbox",
    params(
        ("id" = String, Path, description = "Agent ID (UUID)"),
        InboxQuery
    ),
    responses(
        (status = 200, description = "Message timeline", body = crate::response::Envelope<Vec<crate::types::ApiInboxMessage>>),
        (status = 400, description = "Bad agent id", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Query(q): Query<InboxQuery>,
) -> Result<Json<Envelope<Vec<ApiInboxMessage>>>, ApiError> {
    require_permission(&key, "agents:r")?;
    Ok(Json(Envelope::new(svc.agent_inbox(id, q.limit).await?)))
}
