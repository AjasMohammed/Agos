//! Cost endpoints: summary, per-agent costs.

use axum::extract::{Path, State};
use axum::Json;
use std::sync::Arc;

use crate::error::ApiError;
use crate::service::KernelService;

/// `GET /v1/costs/summary` — Get cost summary across all agents.
pub async fn summary(
    State(svc): State<Arc<dyn KernelService>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entries = svc.get_cost_summary().await?;
    Ok(Json(serde_json::json!({ "costs": entries })))
}

/// `GET /v1/costs/agents/{name}` — Get cost summary for a specific agent.
pub async fn agent_costs(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entry = svc.get_agent_costs(&name).await?;
    Ok(Json(serde_json::json!(entry)))
}
