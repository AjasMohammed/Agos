//! Pipeline endpoints: list, save, run, delete.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ImportPipelineRequest, RunPipelineRequest, SavePipelineRequest};

/// `GET /api/v1/pipelines` — List all saved pipelines.
#[utoipa::path(
    get,
    path = "/api/v1/pipelines",
    tag = "pipelines",
    operation_id = "pipelines_list",
    responses(
        (status = 200, description = "List of pipelines", body = crate::response::Envelope<Vec<crate::types::ApiPipelineSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<crate::types::ApiPipelineSummary>>>, ApiError> {
    require_permission(&key, "pipelines:r")?;
    let pipelines = svc.list_pipelines().await?;
    Ok(Json(Envelope::new(pipelines)))
}

/// `POST /api/v1/pipelines` — Save (create or update) a pipeline.
#[utoipa::path(
    post,
    path = "/api/v1/pipelines",
    tag = "pipelines",
    operation_id = "pipelines_save",
    request_body = SavePipelineRequest,
    responses(
        (status = 200, description = "Pipeline saved", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn save(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<SavePipelineRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "pipelines:w")?;
    svc.save_pipeline(req).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "ok": true }))))
}

/// `POST /api/v1/pipelines/{name}/run` — Execute a pipeline.
#[utoipa::path(
    post,
    path = "/api/v1/pipelines/{name}/run",
    tag = "pipelines",
    operation_id = "pipelines_run",
    params(("name" = String, Path, description = "Pipeline name")),
    request_body = RunPipelineRequest,
    responses(
        (status = 200, description = "Pipeline run result", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Pipeline not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn run(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(mut req): Json<RunPipelineRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "pipelines:w")?;
    req.name = name;
    let result = svc.run_pipeline(req).await?;
    Ok(Json(Envelope::new(result)))
}

/// `DELETE /api/v1/pipelines/{name}` — Delete a pipeline.
#[utoipa::path(
    delete,
    path = "/api/v1/pipelines/{name}",
    tag = "pipelines",
    operation_id = "pipelines_delete",
    params(("name" = String, Path, description = "Pipeline name")),
    responses(
        (status = 200, description = "Pipeline deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Pipeline not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "pipelines:w")?;
    svc.delete_pipeline(&name).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": name }))))
}

/// `POST /api/v1/pipelines/import` — Install a pipeline from raw YAML.
#[utoipa::path(
    post, path = "/api/v1/pipelines/import", tag = "pipelines", operation_id = "pipelines_import",
    request_body = ImportPipelineRequest,
    responses(
        (status = 200, description = "Pipeline imported", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn import(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<ImportPipelineRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "pipelines:w")?;
    let name = svc.import_pipeline(req.yaml).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "imported": name }))))
}

/// `GET /api/v1/pipelines/{name}/export` — Export a pipeline as raw YAML.
#[utoipa::path(
    get, path = "/api/v1/pipelines/{name}/export", tag = "pipelines", operation_id = "pipelines_export",
    params(("name" = String, Path, description = "Pipeline name")),
    responses(
        (status = 200, description = "Pipeline YAML", body = crate::response::Envelope<crate::types::PipelineExport>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Pipeline not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn export(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<crate::types::PipelineExport>>, ApiError> {
    require_permission(&key, "pipelines:r")?;
    let yaml = svc.export_pipeline(&name).await?;
    Ok(Json(Envelope::new(crate::types::PipelineExport {
        name,
        yaml,
    })))
}

/// `GET /api/v1/pipelines/{name}` — Full pipeline definition as JSON.
#[utoipa::path(
    get, path = "/api/v1/pipelines/{name}", tag = "pipelines", operation_id = "pipelines_get",
    params(("name" = String, Path, description = "Pipeline name")),
    responses(
        (status = 200, description = "Pipeline definition", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Pipeline not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "pipelines:r")?;
    let def = svc.get_pipeline_definition(&name).await?;
    Ok(Json(Envelope::new(def)))
}

/// `GET /api/v1/pipelines/runs/{run_id}/events` — Snapshot of a pipeline run.
#[utoipa::path(
    get, path = "/api/v1/pipelines/runs/{run_id}/events", tag = "pipelines", operation_id = "pipelines_run_events",
    params(("run_id" = String, Path, description = "Pipeline run ID")),
    responses(
        (status = 200, description = "Pipeline run snapshot", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Run not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn run_events(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(run_id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "pipelines:r")?;
    let run = svc.get_pipeline_run(run_id).await?;
    Ok(Json(Envelope::new(run)))
}
