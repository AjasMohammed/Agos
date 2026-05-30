//! Webhook endpoint management: list, create, rotate, delete (Phase 05).
//!
//! Distinct from `crate::handlers::webhooks` inbound ingestion (the public
//! Telegram webhook). These routes are protected and manage the webhook
//! endpoint registry.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiWebhookEndpoint, CreateWebhookRequest, WebhookSecretResponse};

/// `GET /api/v1/webhooks` — List webhook endpoints.
#[utoipa::path(
    get,
    path = "/api/v1/webhooks",
    tag = "webhooks",
    operation_id = "webhooks_list",
    responses(
        (status = 200, description = "List of webhook endpoints", body = crate::response::Envelope<Vec<ApiWebhookEndpoint>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiWebhookEndpoint>>>, ApiError> {
    require_permission(&key, "webhooks:r")?;
    Ok(Json(Envelope::new(svc.list_webhooks().await?)))
}

/// `POST /api/v1/webhooks` — Create a webhook endpoint (returns secret once).
#[utoipa::path(
    post,
    path = "/api/v1/webhooks",
    tag = "webhooks",
    operation_id = "webhooks_create",
    request_body = CreateWebhookRequest,
    responses(
        (status = 200, description = "Endpoint created", body = crate::response::Envelope<WebhookSecretResponse>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<CreateWebhookRequest>,
) -> Result<Json<Envelope<WebhookSecretResponse>>, ApiError> {
    require_permission(&key, "webhooks:w")?;
    Ok(Json(Envelope::new(svc.create_webhook(req).await?)))
}

/// `POST /api/v1/webhooks/{id}/rotate` — Rotate the endpoint secret (returns once).
#[utoipa::path(
    post,
    path = "/api/v1/webhooks/{id}/rotate",
    tag = "webhooks",
    operation_id = "webhooks_rotate",
    params(("id" = String, Path, description = "Webhook endpoint id")),
    responses(
        (status = 200, description = "Secret rotated", body = crate::response::Envelope<WebhookSecretResponse>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Endpoint not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn rotate(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<WebhookSecretResponse>>, ApiError> {
    require_permission(&key, "webhooks:w")?;
    Ok(Json(Envelope::new(svc.rotate_webhook(&id).await?)))
}

/// `DELETE /api/v1/webhooks/{id}` — Delete a webhook endpoint.
#[utoipa::path(
    delete,
    path = "/api/v1/webhooks/{id}",
    tag = "webhooks",
    operation_id = "webhooks_delete",
    params(("id" = String, Path, description = "Webhook endpoint id")),
    responses(
        (status = 200, description = "Endpoint deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 400, description = "Invalid endpoint id", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Endpoint not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "webhooks:w")?;
    svc.delete_webhook(&id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": id }))))
}
