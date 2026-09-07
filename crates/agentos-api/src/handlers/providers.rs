//! Read-only LLM provider catalog: what `POST /api/v1/agents` accepts for
//! `provider`, plus each provider's models.

use axum::extract::State;
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::ApiProvider;

/// `GET /api/v1/providers` — List built-in and catalog LLM providers.
#[utoipa::path(
    get,
    path = "/api/v1/providers",
    tag = "agents",
    operation_id = "providers_list",
    responses(
        (status = 200, description = "Available LLM providers", body = crate::response::Envelope<Vec<crate::types::ApiProvider>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiProvider>>>, ApiError> {
    require_permission(&key, "agents:r")?;
    Ok(Json(Envelope::new(svc.list_providers().await?)))
}
