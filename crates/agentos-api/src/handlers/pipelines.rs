//! Pipeline endpoints: list, save, run, delete.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::{RunPipelineRequest, SavePipelineRequest};

/// `GET /api/v1/pipelines` — List all saved pipelines.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "pipelines:r")?;
    let pipelines = svc.list_pipelines().await?;
    Ok(Json(serde_json::json!({ "data": pipelines })))
}

/// `POST /api/v1/pipelines` — Save (create or update) a pipeline.
pub async fn save(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<SavePipelineRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "pipelines:w")?;
    svc.save_pipeline(req).await?;
    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

/// `POST /api/v1/pipelines/{name}/run` — Execute a pipeline.
pub async fn run(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(mut req): Json<RunPipelineRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "pipelines:w")?;
    req.name = name;
    let result = svc.run_pipeline(req).await?;
    Ok(Json(serde_json::json!({ "data": result })))
}

/// `DELETE /api/v1/pipelines/{name}` — Delete a pipeline.
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "pipelines:w")?;
    svc.delete_pipeline(&name).await?;
    Ok(Json(serde_json::json!({ "data": { "deleted": name } })))
}
