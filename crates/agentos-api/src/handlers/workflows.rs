//! Workflow CRUD endpoints: list, get, create, update, delete.
//!
//! Workflows are opaque JSON documents persisted under
//! `<data_dir>/workflows/<id>.json`. Execution is out of scope (see module docs
//! on `crate::types::workflows`).

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiWorkflowSummary, SaveWorkflowRequest, WorkflowSaveResponse};

/// `GET /api/v1/workflows` — List all saved workflows.
#[utoipa::path(
    get,
    path = "/api/v1/workflows",
    tag = "workflows",
    operation_id = "workflows_list",
    responses(
        (status = 200, description = "List of workflows", body = crate::response::Envelope<Vec<crate::types::ApiWorkflowSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiWorkflowSummary>>>, ApiError> {
    require_permission(&key, "workflows:r")?;
    let workflows = svc.list_workflows().await?;
    Ok(Json(Envelope::new(workflows)))
}

/// `GET /api/v1/workflows/{id}` — Fetch a single workflow's full definition.
#[utoipa::path(
    get,
    path = "/api/v1/workflows/{id}",
    tag = "workflows",
    operation_id = "workflows_get",
    params(("id" = String, Path, description = "Workflow ID")),
    responses(
        (status = 200, description = "Workflow definition", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Workflow not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "workflows:r")?;
    let workflow = svc.get_workflow(&id).await?;
    Ok(Json(Envelope::new(workflow)))
}

/// `POST /api/v1/workflows` — Create a new workflow (server assigns the id).
#[utoipa::path(
    post,
    path = "/api/v1/workflows",
    tag = "workflows",
    operation_id = "workflows_create",
    request_body = SaveWorkflowRequest,
    responses(
        (status = 200, description = "Workflow created", body = crate::response::Envelope<crate::types::WorkflowSaveResponse>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<SaveWorkflowRequest>,
) -> Result<Json<Envelope<WorkflowSaveResponse>>, ApiError> {
    require_permission(&key, "workflows:w")?;
    let id = svc.save_workflow(req).await?;
    Ok(Json(Envelope::new(WorkflowSaveResponse { id })))
}

/// `PUT /api/v1/workflows/{id}` — Update an existing workflow in place.
#[utoipa::path(
    put,
    path = "/api/v1/workflows/{id}",
    tag = "workflows",
    operation_id = "workflows_update",
    params(("id" = String, Path, description = "Workflow ID")),
    request_body = SaveWorkflowRequest,
    responses(
        (status = 200, description = "Workflow updated", body = crate::response::Envelope<crate::types::WorkflowSaveResponse>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
    Json(mut req): Json<SaveWorkflowRequest>,
) -> Result<Json<Envelope<WorkflowSaveResponse>>, ApiError> {
    require_permission(&key, "workflows:w")?;
    // Pin the id from the path into the definition so save_workflow updates in place.
    if let serde_json::Value::Object(map) = &mut req.definition {
        map.insert("id".to_string(), serde_json::Value::String(id.clone()));
    }
    let saved_id = svc.save_workflow(req).await?;
    Ok(Json(Envelope::new(WorkflowSaveResponse { id: saved_id })))
}

/// `DELETE /api/v1/workflows/{id}` — Delete a workflow.
#[utoipa::path(
    delete,
    path = "/api/v1/workflows/{id}",
    tag = "workflows",
    operation_id = "workflows_delete",
    params(("id" = String, Path, description = "Workflow ID")),
    responses(
        (status = 200, description = "Workflow deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Workflow not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "workflows:w")?;
    svc.delete_workflow(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": id }))))
}
