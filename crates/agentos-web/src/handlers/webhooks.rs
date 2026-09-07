use crate::state::AppState;
use agentos_kernel::webhook_verify::{ingest_webhook, WebhookIngestError};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use std::collections::HashMap;
use std::sync::Arc;

/// POST /api/v1/webhooks/incoming/:endpoint_id
///
/// Receives external webhook payloads from services like GitHub, Stripe, etc.
///
/// This endpoint is **unauthenticated** — external services cannot carry our
/// session cookie or bearer token. Security is enforced via the webhook secret
/// and provider-specific signature verification, inside
/// [`agentos_kernel::webhook_verify::ingest_webhook`] (shared with `agentos-api`).
pub async fn incoming_webhook(
    State(state): State<Arc<AppState>>,
    Path(endpoint_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_lowercase(), v.to_string()))
        })
        .collect();
    match ingest_webhook(
        &state.kernel.webhook_registry,
        &state.kernel.webhook_throttle,
        &state.kernel.webhook_batcher,
        &endpoint_id,
        header_map,
        &body,
    )
    .await
    {
        Ok(()) => StatusCode::OK,
        Err(WebhookIngestError::NotFound) => StatusCode::NOT_FOUND,
        Err(WebhookIngestError::RateLimited) => StatusCode::TOO_MANY_REQUESTS,
        Err(WebhookIngestError::UnknownProvider) => StatusCode::INTERNAL_SERVER_ERROR,
        Err(WebhookIngestError::InvalidSignature) => StatusCode::UNAUTHORIZED,
    }
}
