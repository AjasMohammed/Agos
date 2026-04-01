//! Secret endpoints: list, set, revoke.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::SetSecretRequest;

/// `GET /api/v1/secrets` — List all secrets (metadata only, no values).
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "secrets:r")?;
    let secrets = svc.list_secrets().await?;
    Ok(Json(serde_json::json!({ "data": secrets })))
}

/// `POST /api/v1/secrets` — Set or update a secret.
pub async fn set(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(mut req): Json<SetSecretRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "secrets:w")?;
    // C5: Take ownership of the secret value and clear the original
    let mut value = std::mem::take(&mut req.value);
    let result = svc.set_secret(req).await;
    // Zero out the value in memory
    value.clear();
    value.shrink_to_fit();
    result?;
    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

/// `DELETE /api/v1/secrets/{name}` — Revoke (delete) a secret.
pub async fn revoke(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&key, "secrets:w")?;
    svc.revoke_secret(&name).await?;
    Ok(Json(serde_json::json!({ "data": { "revoked": name } })))
}
