//! Agent identity endpoint (read-only).

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::ApiAgentIdentity;

/// `GET /api/v1/agents/{name}/identity` — Agent cryptographic identity.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{name}/identity",
    tag = "agents",
    operation_id = "agents_identity",
    params(("name" = String, Path, description = "Agent name")),
    responses(
        (status = 200, description = "Agent identity", body = crate::response::Envelope<ApiAgentIdentity>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<ApiAgentIdentity>>, ApiError> {
    require_permission(&key, "agents:r")?;
    Ok(Json(Envelope::new(svc.get_agent_identity(&name).await?)))
}
