//! Read-only agent-memory browser: list/search one memory tier for an agent.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiMemoryItem, MemoryQuery};

/// `GET /api/v1/agents/{id}/memory/{tier}` — Browse or search an agent's memory.
///
/// `tier` is one of `episodic` / `semantic` / `procedural`. With `?q=` set the
/// tier is searched (FTS/embedding, ranked); without it the most-recent items
/// are returned. Read-only.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{id}/memory/{tier}",
    tag = "memory",
    operation_id = "agent_memory_browse",
    params(
        ("id" = String, Path, description = "Agent ID (UUID)"),
        ("tier" = String, Path, description = "Memory tier: episodic | semantic | procedural"),
        MemoryQuery
    ),
    responses(
        (status = 200, description = "Memory items", body = crate::response::Envelope<Vec<crate::types::ApiMemoryItem>>),
        (status = 400, description = "Bad agent id or unknown tier", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn browse(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path((id, tier)): Path<(String, String)>,
    Query(q): Query<MemoryQuery>,
) -> Result<Json<Envelope<Vec<ApiMemoryItem>>>, ApiError> {
    require_permission(&key, "memory:r")?;
    let items = svc.browse_agent_memory(id, tier, q.q, q.limit).await?;
    Ok(Json(Envelope::new(items)))
}
