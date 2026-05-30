//! Secret endpoints: list, set, revoke.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::SetSecretRequest;

/// `GET /api/v1/secrets` — List all secrets (metadata only, no values).
#[utoipa::path(
    get, path = "/api/v1/secrets", tag = "secrets", operation_id = "secrets_list",
    responses(
        (status = 200, description = "Secret metadata", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "secrets:r")?;
    let secrets = svc.list_secrets().await?;
    Ok(Json(Envelope::new(serde_json::json!(secrets))))
}

/// `POST /api/v1/secrets` — Set or update a secret.
#[utoipa::path(
    post, path = "/api/v1/secrets", tag = "secrets", operation_id = "secrets_set",
    request_body = crate::types::SetSecretRequest,
    responses(
        (status = 200, description = "Secret set", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn set(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(mut req): Json<SetSecretRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "secrets:w")?;
    // C5: Take ownership of the secret value and clear the original
    let mut value = std::mem::take(&mut req.value);
    let result = svc.set_secret(req).await;
    // Zero out the value in memory
    value.clear();
    value.shrink_to_fit();
    result?;
    Ok(Json(Envelope::new(serde_json::json!({ "ok": true }))))
}

/// `DELETE /api/v1/secrets/{name}` — Revoke (delete) a secret.
#[utoipa::path(
    delete, path = "/api/v1/secrets/{name}", tag = "secrets", operation_id = "secrets_revoke",
    params(("name" = String, Path, description = "Secret name")),
    responses(
        (status = 200, description = "Secret revoked", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Secret not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "secrets:w")?;
    svc.revoke_secret(&name).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "revoked": name }))))
}
