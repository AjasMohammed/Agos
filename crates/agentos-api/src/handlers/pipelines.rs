//! Pipeline endpoints: list, save, run, delete.

use axum::extract::{Path, State};
use axum::Json;
use std::sync::Arc;

use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::{RunPipelineRequest, SavePipelineRequest};

/// `GET /v1/pipelines` — List all saved pipelines.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pipelines = svc.list_pipelines().await?;
    Ok(Json(serde_json::json!({ "pipelines": pipelines })))
}

/// `POST /v1/pipelines` — Save (create or update) a pipeline.
pub async fn save(
    State(svc): State<Arc<dyn KernelService>>,
    Json(req): Json<SavePipelineRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    svc.save_pipeline(req).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /v1/pipelines/{name}/run` — Execute a pipeline.
pub async fn run(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
    Json(mut req): Json<RunPipelineRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    req.name = name;
    let result = svc.run_pipeline(req).await?;
    Ok(Json(serde_json::json!({ "result": result })))
}

/// `DELETE /v1/pipelines/{name}` — Delete a pipeline.
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    svc.delete_pipeline(&name).await?;
    Ok(Json(serde_json::json!({ "deleted": name })))
}
