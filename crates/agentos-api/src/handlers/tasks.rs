//! Task endpoints: list, get, run, cancel, trace.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::{Envelope, ListEnvelope};
use crate::service::KernelService;
use crate::types::{RunTaskRequest, TaskFilter};

/// `GET /api/v1/tasks` — List tasks with optional filtering.
#[utoipa::path(
    get, path = "/api/v1/tasks", tag = "tasks", operation_id = "tasks_list",
    params(crate::types::TaskFilter),
    responses(
        (status = 200, description = "Tasks", body = crate::response::ListEnvelope<crate::types::ApiTaskSummary>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(filter): Query<TaskFilter>,
) -> Result<Json<ListEnvelope<crate::types::ApiTaskSummary>>, ApiError> {
    require_permission(&key, "tasks:r")?;
    let (tasks, total) = svc.list_tasks(filter).await?;
    Ok(Json(ListEnvelope::new(tasks, total)))
}

/// `GET /api/v1/tasks/{id}` — Get a single task by ID.
#[utoipa::path(
    get, path = "/api/v1/tasks/{id}", tag = "tasks", operation_id = "tasks_get",
    params(("id" = String, Path, description = "Task ID")),
    responses(
        (status = 200, description = "Task detail", body = crate::response::Envelope<crate::types::ApiTaskDetail>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Task not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<crate::types::ApiTaskDetail>>, ApiError> {
    require_permission(&key, "tasks:r")?;
    let task_id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid task ID: {id}")))?;
    let task = svc.get_task(task_id).await?;
    Ok(Json(Envelope::new(task)))
}

/// `POST /api/v1/tasks/run` — Submit a new task for execution.
#[utoipa::path(
    post, path = "/api/v1/tasks/run", tag = "tasks", operation_id = "tasks_run",
    request_body = crate::types::RunTaskRequest,
    responses(
        (status = 200, description = "Task submitted", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn run(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<RunTaskRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "tasks:w")?;
    let task_id = svc.run_task(req).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "task_id": task_id.to_string() }),
    )))
}

/// `POST /api/v1/tasks/{id}/cancel` — Cancel a running task.
#[utoipa::path(
    post, path = "/api/v1/tasks/{id}/cancel", tag = "tasks", operation_id = "tasks_cancel",
    params(("id" = String, Path, description = "Task ID")),
    responses(
        (status = 200, description = "Task cancelled", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Task not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "tasks:w")?;
    let task_id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid task ID: {id}")))?;
    svc.cancel_task(task_id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "cancelled": id }))))
}

/// `GET /api/v1/tasks/{id}/trace` — Get execution trace for a task.
#[utoipa::path(
    get, path = "/api/v1/tasks/{id}/trace", tag = "tasks", operation_id = "tasks_trace",
    params(("id" = String, Path, description = "Task ID")),
    responses(
        (status = 200, description = "Task trace", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Task not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn trace(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "tasks:r")?;
    let task_id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid task ID: {id}")))?;
    let trace = svc.get_task_trace(task_id).await?;
    Ok(Json(Envelope::new(serde_json::json!(trace))))
}

/// `POST /api/v1/tasks/{id}/resume` — Resume a task from its latest checkpoint.
#[utoipa::path(
    post, path = "/api/v1/tasks/{id}/resume", tag = "tasks", operation_id = "tasks_resume",
    params(("id" = String, Path, description = "Task ID")),
    responses(
        (status = 200, description = "Task resumed", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Task or checkpoint not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn resume(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "tasks:w")?;
    let task_id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid task ID: {id}")))?;
    let data = svc.resume_task(task_id).await?;
    Ok(Json(Envelope::new(data)))
}

/// `GET /api/v1/tasks/{id}/checkpoints` — List checkpoints for a task (0 or 1).
#[utoipa::path(
    get, path = "/api/v1/tasks/{id}/checkpoints", tag = "tasks", operation_id = "tasks_checkpoints",
    params(("id" = String, Path, description = "Task ID")),
    responses(
        (status = 200, description = "Checkpoints", body = crate::response::Envelope<Vec<crate::types::ApiCheckpointSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Task not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn checkpoints(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Vec<crate::types::ApiCheckpointSummary>>>, ApiError> {
    require_permission(&key, "tasks:r")?;
    let task_id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("Invalid task ID: {id}")))?;
    let cps = svc.list_task_checkpoints(task_id).await?;
    Ok(Json(Envelope::new(cps)))
}
