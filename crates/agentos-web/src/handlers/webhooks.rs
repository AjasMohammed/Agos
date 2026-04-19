use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use std::collections::HashMap;
use std::sync::Arc;

/// Convert Axum `HeaderMap` to a `HashMap<String, String>` with lowercased keys.
/// Only includes headers relevant for webhook signature verification.
fn headermap_to_hashmap(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_lowercase(), v.to_string()))
        })
        .collect()
}

/// POST /api/v1/webhooks/incoming/:endpoint_id
///
/// Receives external webhook payloads from services like GitHub, Stripe, etc.
///
/// This endpoint is **unauthenticated** — external services cannot carry our
/// session cookie or bearer token. Security is enforced via the webhook secret
/// and provider-specific signature verification.
///
/// The handler returns 200 OK immediately and enqueues the event for async
/// processing via the webhook batcher.
pub async fn incoming_webhook(
    State(state): State<Arc<AppState>>,
    Path(endpoint_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    // 1. Parse and look up endpoint (with secret for signature verification)
    let endpoint_uuid = match endpoint_id.parse() {
        Ok(id) => id,
        Err(_) => return StatusCode::NOT_FOUND,
    };

    let (endpoint_meta, secret) = match state
        .kernel
        .webhook_registry
        .get_endpoint_with_secret(&endpoint_uuid)
        .await
    {
        Some((meta, secret)) if meta.active => (meta, secret),
        _ => return StatusCode::NOT_FOUND,
    };

    // 2. Rate-limit check
    if !state.kernel.webhook_throttle.allow(&endpoint_uuid).await {
        tracing::warn!(endpoint_id = %endpoint_id, "Webhook rate-limited");
        return StatusCode::TOO_MANY_REQUESTS;
    }

    // 3. Parse the provider string back to enum for verification dispatch
    let provider: agentos_types::WebhookProvider =
        match serde_json::from_value(serde_json::json!(endpoint_meta.provider)) {
            Ok(p) => p,
            Err(_) => {
                tracing::error!(
                    endpoint_id = %endpoint_id,
                    provider = %endpoint_meta.provider,
                    "Unknown webhook provider"
                );
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        };

    // 4. Verify signature
    let header_map = headermap_to_hashmap(&headers);
    let sig_valid = match agentos_kernel::webhook_verify::verify_webhook_signature(
        &provider,
        &secret,
        &body,
        &header_map,
    ) {
        Ok(valid) => valid,
        Err(e) => {
            tracing::warn!(
                endpoint_id = %endpoint_id,
                error = %e,
                "Signature verification error"
            );
            false
        }
    };

    if !sig_valid {
        tracing::warn!(endpoint_id = %endpoint_id, "Invalid webhook signature");
        return StatusCode::UNAUTHORIZED;
    }

    // 5. Parse body as JSON (or wrap raw bytes)
    let payload = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(json) => json,
        Err(_) => {
            serde_json::json!({ "_raw": String::from_utf8_lossy(&body) })
        }
    };

    // 6. Build event with only safe headers (no auth/cookie/secret headers).
    // Allow known provider event-metadata headers; deny auth/credential patterns
    // that would expose secrets to downstream agents and the audit log.
    let safe_headers: HashMap<String, String> = header_map
        .into_iter()
        .filter(|(name, _)| {
            if name == "content-type" || name == "user-agent" {
                return true;
            }
            if !name.starts_with("x-") {
                return false;
            }
            // Denylist x-* headers that carry credentials or secrets.
            !matches!(
                name.as_str(),
                "x-api-key"
                    | "x-auth-token"
                    | "x-access-token"
                    | "x-secret"
                    | "x-secret-key"
                    | "x-private-key"
                    | "x-password"
                    | "x-token"
                    | "x-authorization"
            ) && !name.starts_with("x-auth-")
                && !name.starts_with("x-secret-")
                && !name.starts_with("x-access-")
        })
        .collect();

    let webhook_event = agentos_types::WebhookEvent {
        endpoint_id: endpoint_uuid,
        provider: provider.clone(),
        headers: safe_headers,
        payload,
        received_at: chrono::Utc::now(),
        signature_valid: true,
    };

    // 7. Record receipt in registry (best-effort)
    if let Err(e) = state
        .kernel
        .webhook_registry
        .record_receipt(&endpoint_uuid)
        .await
    {
        tracing::warn!(
            endpoint_id = %endpoint_id,
            error = %e,
            "Failed to record webhook receipt"
        );
    }

    // 8. Enqueue in batcher for debounced processing
    state
        .kernel
        .webhook_batcher
        .add_event(
            webhook_event,
            endpoint_meta.agent_id,
            provider,
            endpoint_meta.debounce_seconds,
        )
        .await;

    tracing::debug!(endpoint_id = %endpoint_id, "Webhook received and enqueued");
    StatusCode::OK
}
