//! Cost endpoints: summary, per-agent costs.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;

/// `GET /api/v1/costs/summary` — Get cost summary across all agents.
pub async fn summary(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "costs:r")?;
    let entries = svc.get_cost_summary().await?;
    Ok(Json(serde_json::json!({ "data": entries })))
}

/// `GET /api/v1/costs/agents/{name}` — Get cost summary for a specific agent.
pub async fn agent_costs(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "costs:r")?;
    let entry = svc.get_agent_costs(&name).await?;
    Ok(Json(serde_json::json!({ "data": entry })))
}
