//! Scratchpad endpoints: global + per-agent page list/read/write/delete.
//!
//! The global scratchpad has no kernel namespace, so the global routes use the
//! reserved `GLOBAL_AGENT_ID` sentinel; the per-agent routes pass the agent
//! name straight through. Both variants share one set of `KernelService`
//! methods — only the `agent_id` argument differs.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiScratchPage, SavePageRequest, ScratchListResponse};

/// Reserved agent id for the global (non-namespaced) scratchpad.
pub const GLOBAL_AGENT_ID: &str = "__global__";

// ── Global scratchpad ────────────────────────────────────────────────────────

/// `GET /api/v1/scratchpad` — List pages in the global scratchpad.
#[utoipa::path(
    get,
    path = "/api/v1/scratchpad",
    tag = "scratchpad",
    operation_id = "scratchpad_list_global",
    responses(
        (status = 200, description = "Global scratchpad pages", body = crate::response::Envelope<crate::types::ScratchListResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_global(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<ScratchListResponse>>, ApiError> {
    require_permission(&key, "scratchpad:r")?;
    let pages = svc.get_scratchpad(GLOBAL_AGENT_ID).await?;
    Ok(Json(Envelope::new(ScratchListResponse { pages })))
}

/// `GET /api/v1/scratchpad/{page}` — Read a page from the global scratchpad.
#[utoipa::path(
    get,
    path = "/api/v1/scratchpad/{page}",
    tag = "scratchpad",
    operation_id = "scratchpad_get_global",
    params(("page" = String, Path, description = "Page title")),
    responses(
        (status = 200, description = "Page", body = crate::response::Envelope<crate::types::ApiScratchPage>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_global(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(page): Path<String>,
) -> Result<Json<Envelope<ApiScratchPage>>, ApiError> {
    require_permission(&key, "scratchpad:r")?;
    let p = svc.get_scratchpad_page(GLOBAL_AGENT_ID, &page).await?;
    Ok(Json(Envelope::new(p)))
}

/// `PUT /api/v1/scratchpad/{page}` — Create or overwrite a global page.
#[utoipa::path(
    put,
    path = "/api/v1/scratchpad/{page}",
    tag = "scratchpad",
    operation_id = "scratchpad_put_global",
    params(("page" = String, Path, description = "Page title")),
    request_body = SavePageRequest,
    responses(
        (status = 200, description = "Saved page", body = crate::response::Envelope<crate::types::ApiScratchPage>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn put_global(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(page): Path<String>,
    Json(req): Json<SavePageRequest>,
) -> Result<Json<Envelope<ApiScratchPage>>, ApiError> {
    require_permission(&key, "scratchpad:w")?;
    let p = svc
        .save_scratchpad_page(GLOBAL_AGENT_ID, &page, req.content, req.tags)
        .await?;
    Ok(Json(Envelope::new(p)))
}

/// `DELETE /api/v1/scratchpad/{page}` — Delete a global page.
#[utoipa::path(
    delete,
    path = "/api/v1/scratchpad/{page}",
    tag = "scratchpad",
    operation_id = "scratchpad_delete_global",
    params(("page" = String, Path, description = "Page title")),
    responses(
        (status = 200, description = "Page deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_global(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(page): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "scratchpad:w")?;
    svc.delete_scratchpad_page(GLOBAL_AGENT_ID, &page).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": page }))))
}

// ── Per-agent scratchpad ─────────────────────────────────────────────────────

/// `GET /api/v1/agents/{name}/scratchpad` — List an agent's scratchpad pages.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{name}/scratchpad",
    tag = "scratchpad",
    operation_id = "scratchpad_list_agent",
    params(("name" = String, Path, description = "Agent name / id")),
    responses(
        (status = 200, description = "Agent scratchpad pages", body = crate::response::Envelope<crate::types::ScratchListResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_agent(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<ScratchListResponse>>, ApiError> {
    require_permission(&key, "scratchpad:r")?;
    let pages = svc.get_scratchpad(&name).await?;
    Ok(Json(Envelope::new(ScratchListResponse { pages })))
}

/// `GET /api/v1/agents/{name}/scratchpad/{page}` — Read an agent's page.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{name}/scratchpad/{page}",
    tag = "scratchpad",
    operation_id = "scratchpad_get_agent",
    params(
        ("name" = String, Path, description = "Agent name / id"),
        ("page" = String, Path, description = "Page title")
    ),
    responses(
        (status = 200, description = "Page", body = crate::response::Envelope<crate::types::ApiScratchPage>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_agent(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path((name, page)): Path<(String, String)>,
) -> Result<Json<Envelope<ApiScratchPage>>, ApiError> {
    require_permission(&key, "scratchpad:r")?;
    let p = svc.get_scratchpad_page(&name, &page).await?;
    Ok(Json(Envelope::new(p)))
}

/// `PUT /api/v1/agents/{name}/scratchpad/{page}` — Create or overwrite a page.
#[utoipa::path(
    put,
    path = "/api/v1/agents/{name}/scratchpad/{page}",
    tag = "scratchpad",
    operation_id = "scratchpad_put_agent",
    params(
        ("name" = String, Path, description = "Agent name / id"),
        ("page" = String, Path, description = "Page title")
    ),
    request_body = SavePageRequest,
    responses(
        (status = 200, description = "Saved page", body = crate::response::Envelope<crate::types::ApiScratchPage>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn put_agent(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path((name, page)): Path<(String, String)>,
    Json(req): Json<SavePageRequest>,
) -> Result<Json<Envelope<ApiScratchPage>>, ApiError> {
    require_permission(&key, "scratchpad:w")?;
    let p = svc
        .save_scratchpad_page(&name, &page, req.content, req.tags)
        .await?;
    Ok(Json(Envelope::new(p)))
}

/// `DELETE /api/v1/agents/{name}/scratchpad/{page}` — Delete a page.
#[utoipa::path(
    delete,
    path = "/api/v1/agents/{name}/scratchpad/{page}",
    tag = "scratchpad",
    operation_id = "scratchpad_delete_agent",
    params(
        ("name" = String, Path, description = "Agent name / id"),
        ("page" = String, Path, description = "Page title")
    ),
    responses(
        (status = 200, description = "Page deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_agent(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path((name, page)): Path<(String, String)>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "scratchpad:w")?;
    svc.delete_scratchpad_page(&name, &page).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": page }))))
}
