//! Cost endpoints: summary, per-agent costs.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;

/// `GET /api/v1/costs/summary` — Get cost summary across all agents.
#[utoipa::path(
    get, path = "/api/v1/costs/summary", tag = "costs", operation_id = "costs_summary",
    responses(
        (status = 200, description = "Cost summary entries", body = crate::response::Envelope<Vec<crate::types::CostSummaryEntry>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn summary(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<crate::types::CostSummaryEntry>>>, ApiError> {
    require_permission(&key, "costs:r")?;
    let entries = svc.get_cost_summary().await?;
    Ok(Json(Envelope::new(entries)))
}

/// `GET /api/v1/costs/agents/{name}` — Get cost summary for a specific agent.
#[utoipa::path(
    get, path = "/api/v1/costs/agents/{name}", tag = "costs", operation_id = "costs_agent_costs",
    params(("name" = String, Path, description = "Agent name")),
    responses(
        (status = 200, description = "Cost summary for an agent", body = crate::response::Envelope<crate::types::CostSummaryEntry>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Agent not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn agent_costs(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<crate::types::CostSummaryEntry>>, ApiError> {
    require_permission(&key, "costs:r")?;
    let entry = svc.get_agent_costs(&name).await?;
    Ok(Json(Envelope::new(entry)))
}
