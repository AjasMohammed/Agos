//! Task endpoints: list, get, run, cancel, trace.

use axum::extract::{Path, Query, State};
use axum::Json;
use std::sync::Arc;

use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::{RunTaskRequest, TaskFilter};

/// `GET /v1/tasks` — List tasks with optional filtering.
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Query(filter): Query<TaskFilter>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (tasks, total) = svc.list_tasks(filter).await?;
    Ok(Json(serde_json::json!({ "tasks": tasks, "total": total })))
}

/// `GET /v1/tasks/{id}` — Get a single task by ID.
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let task_id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid task ID: {id}")))?;
    let task = svc.get_task(task_id).await?;
    Ok(Json(serde_json::json!(task)))
}

/// `POST /v1/tasks/run` — Submit a new task for execution.
pub async fn run(
    State(svc): State<Arc<dyn KernelService>>,
    Json(req): Json<RunTaskRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let task_id = svc.run_task(req).await?;
    Ok(Json(serde_json::json!({ "task_id": task_id.to_string() })))
}

/// `POST /v1/tasks/{id}/cancel` — Cancel a running task.
pub async fn cancel(
    State(svc): State<Arc<dyn KernelService>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let task_id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid task ID: {id}")))?;
    svc.cancel_task(task_id).await?;
    Ok(Json(serde_json::json!({ "cancelled": id })))
}

/// `GET /v1/tasks/{id}/trace` — Get execution trace for a task.
pub async fn trace(
    State(svc): State<Arc<dyn KernelService>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let task_id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid task ID: {id}")))?;
    let trace = svc.get_task_trace(task_id).await?;
    Ok(Json(serde_json::json!(trace)))
}
