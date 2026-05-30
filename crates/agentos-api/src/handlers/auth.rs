//! Browser-auth endpoints: operator login, key refresh, and identity.
//!
//! `POST /auth/login` exchanges the operator credential (`[api] operator_token`)
//! for a scoped, expiring `agos_` key — the SPA's entry point. `GET /auth/me`
//! reports the presented key's identity/scopes. `POST /auth/refresh` (gated by
//! `[api] refresh_enabled`) rotates a valid key.

use axum::extract::State;
use axum::Extension;
use axum::Json;
use chrono::{Duration, Utc};
use std::sync::Arc;
use zeroize::Zeroizing;

use crate::api_key::ApiKeyStore;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::{CredentialCheck, KernelService};
use crate::types::{AuthMe, IssuedKeyResponse, LoginRequest};
use agentos_audit::AuditEventType;

/// Default scopes granted to a key minted via operator login (full access).
const OPERATOR_SCOPES: &[&str] = &["*:rw"];
/// Lifetime of a login / refresh key, in seconds (24h working session).
const LOGIN_KEY_TTL_SECS: i64 = 24 * 60 * 60;

fn operator_scopes() -> Vec<String> {
    OPERATOR_SCOPES.iter().map(|s| s.to_string()).collect()
}

/// `POST /api/v1/auth/login` — Exchange the operator credential for a scoped,
/// expiring API key. Public, but heavily rate-limited to resist brute force.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "auth",
    operation_id = "auth_login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Minted API key (shown once)", body = crate::response::Envelope<crate::types::IssuedKeyResponse>),
        (status = 401, description = "Invalid credential", body = crate::error::ApiErrorBody),
        (status = 503, description = "Login disabled (no operator credential configured)", body = crate::error::ApiErrorBody)
    )
)]
pub async fn login(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(store): Extension<ApiKeyStore>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<Envelope<IssuedKeyResponse>>, ApiError> {
    // Move the credential into a zeroizing wrapper so it is cleared on drop.
    let credential = Zeroizing::new(req.credential);

    match svc.verify_operator_credential(&credential).await {
        CredentialCheck::NotConfigured => Err(ApiError::ServiceUnavailable(
            "Login is disabled: no operator credential is configured".into(),
        )),
        CredentialCheck::Invalid => {
            svc.record_audit(
                AuditEventType::ApiLoginFailed,
                serde_json::json!({ "reason": "invalid_credential" }),
            )
            .await;
            Err(ApiError::Unauthorized)
        }
        CredentialCheck::Valid => {
            let expires_at = Some(Utc::now() + Duration::seconds(LOGIN_KEY_TTL_SECS));
            let scopes = operator_scopes();
            let issued = store
                .issue("operator-login".into(), scopes.clone(), expires_at)
                .await;
            svc.record_audit(
                AuditEventType::ApiLoginSucceeded,
                serde_json::json!({ "key_id": issued.key_id }),
            )
            .await;
            svc.record_audit(
                AuditEventType::ApiKeyIssued,
                serde_json::json!({ "key_id": issued.key_id, "name": "operator-login", "via": "login" }),
            )
            .await;
            Ok(Json(Envelope::new(IssuedKeyResponse {
                api_key: issued.api_key,
                key_id: issued.key_id,
                name: issued.record.name,
                scopes,
                expires_at,
            })))
        }
    }
}

/// `GET /api/v1/auth/me` — Identity and scopes of the presented key.
#[utoipa::path(
    get,
    path = "/api/v1/auth/me",
    tag = "auth",
    operation_id = "auth_me",
    responses(
        (status = 200, description = "Identity of the presented key", body = crate::response::Envelope<crate::types::AuthMe>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn me(
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<AuthMe>>, ApiError> {
    let r = &key.0;
    Ok(Json(Envelope::new(AuthMe {
        key_id: r.id.clone(),
        name: r.name.clone(),
        scopes: r.permissions.clone(),
        expires_at: r.expires_at,
    })))
}

/// `POST /api/v1/auth/refresh` — Rotate the presented key: mint a fresh key with
/// the same scopes and a new TTL, and revoke the old one. Gated by
/// `[api] refresh_enabled` (the route is absent when disabled).
#[utoipa::path(
    post,
    path = "/api/v1/auth/refresh",
    tag = "auth",
    operation_id = "auth_refresh",
    responses(
        (status = 200, description = "Rotated API key (shown once)", body = crate::response::Envelope<crate::types::IssuedKeyResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn refresh(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(store): Extension<ApiKeyStore>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<IssuedKeyResponse>>, ApiError> {
    let old = &key.0;
    let scopes = old.permissions.clone();
    let expires_at = Some(Utc::now() + Duration::seconds(LOGIN_KEY_TTL_SECS));
    let issued = store
        .issue(old.name.clone(), scopes.clone(), expires_at)
        .await;
    // Revoke the old key by its public id.
    store.revoke_by_id(&old.id).await;

    svc.record_audit(
        AuditEventType::ApiKeyIssued,
        serde_json::json!({ "key_id": issued.key_id, "name": old.name, "via": "refresh" }),
    )
    .await;
    svc.record_audit(
        AuditEventType::ApiKeyRevoked,
        serde_json::json!({ "key_id": old.id, "via": "refresh" }),
    )
    .await;

    Ok(Json(Envelope::new(IssuedKeyResponse {
        api_key: issued.api_key,
        key_id: issued.key_id,
        name: issued.record.name,
        scopes,
        expires_at,
    })))
}
