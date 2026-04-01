//! Secret endpoints: list, set, revoke.

use axum::extract::{Path, State};
use axum::Json;
use std::sync::Arc;

use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::SetSecretRequest;

/// `GET /v1/secrets` — List all secrets (metadata only, no values).
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let secrets = svc.list_secrets().await?;
    Ok(Json(serde_json::json!({ "secrets": secrets })))
}

/// `POST /v1/secrets` — Set or update a secret.
pub async fn set(
    State(svc): State<Arc<dyn KernelService>>,
    Json(req): Json<SetSecretRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    svc.set_secret(req).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `DELETE /v1/secrets/{name}` — Revoke (delete) a secret.
pub async fn revoke(
    State(svc): State<Arc<dyn KernelService>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    svc.revoke_secret(&name).await?;
    Ok(Json(serde_json::json!({ "revoked": name })))
}
