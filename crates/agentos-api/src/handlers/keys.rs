//! API key management endpoints (operator-scoped, `keys:rw`).
//!
//! Lists key metadata, mints new scoped keys (raw secret shown once), and
//! revokes keys by their public id. Key material is never returned by `list`.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use chrono::{Duration, Utc};
use std::sync::Arc;

use super::require_permission;
use crate::api_key::ApiKeyStore;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiKeyMeta, CreateKeyRequest, IssuedKeyResponse};
use agentos_audit::AuditEventType;

/// `GET /api/v1/keys` — List all API keys with management metadata (never the
/// raw key or hash).
#[utoipa::path(
    get,
    path = "/api/v1/keys",
    tag = "keys",
    operation_id = "keys_list",
    responses(
        (status = 200, description = "API key metadata", body = crate::response::Envelope<Vec<crate::types::ApiKeyMeta>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 403, description = "Insufficient scope", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    Extension(store): Extension<ApiKeyStore>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiKeyMeta>>>, ApiError> {
    require_permission(&key, "keys:r")?;
    Ok(Json(Envelope::new(store.list_all().await)))
}

/// `POST /api/v1/keys` — Mint a new scoped API key. The raw key is returned once.
#[utoipa::path(
    post,
    path = "/api/v1/keys",
    tag = "keys",
    operation_id = "keys_create",
    request_body = CreateKeyRequest,
    responses(
        (status = 200, description = "Minted API key (shown once)", body = crate::response::Envelope<crate::types::IssuedKeyResponse>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 403, description = "Insufficient scope", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(store): Extension<ApiKeyStore>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<CreateKeyRequest>,
) -> Result<Json<Envelope<IssuedKeyResponse>>, ApiError> {
    require_permission(&key, "keys:w")?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("Key name must not be empty".into()));
    }
    let expires_at = req
        .ttl_secs
        .map(|secs| Utc::now() + Duration::seconds(secs as i64));
    let issued = store
        .issue(req.name.clone(), req.scopes.clone(), expires_at)
        .await;
    svc.record_audit(
        AuditEventType::ApiKeyIssued,
        serde_json::json!({ "key_id": issued.key_id, "name": req.name, "via": "keys_create" }),
    )
    .await;
    Ok(Json(Envelope::new(IssuedKeyResponse {
        api_key: issued.api_key,
        key_id: issued.key_id,
        name: issued.record.name,
        scopes: req.scopes,
        expires_at,
    })))
}

/// `DELETE /api/v1/keys/{id}` — Revoke a key by its public id. Idempotent-ish:
/// returns 404 if no key with that id exists. Keys are never hard-deleted.
#[utoipa::path(
    delete,
    path = "/api/v1/keys/{id}",
    tag = "keys",
    operation_id = "keys_revoke",
    params(("id" = String, Path, description = "Public key id")),
    responses(
        (status = 200, description = "Key revoked", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 403, description = "Insufficient scope", body = crate::error::ApiErrorBody),
        (status = 404, description = "Key not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(store): Extension<ApiKeyStore>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "keys:w")?;
    if !store.revoke_by_id(&id).await {
        return Err(ApiError::NotFound(format!("No API key with id {id}")));
    }
    svc.record_audit(
        AuditEventType::ApiKeyRevoked,
        serde_json::json!({ "key_id": id, "via": "keys_revoke" }),
    )
    .await;
    Ok(Json(Envelope::new(serde_json::json!({ "revoked": id }))))
}
