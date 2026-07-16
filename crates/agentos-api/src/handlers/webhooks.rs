//! Webhook ingestion endpoints for external channel adapters (Telegram, etc.).
//!
//! These routes are **public** (no API key required) — authentication is handled
//! via adapter-specific mechanisms (e.g. Telegram's `secret_token` header).

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use std::collections::HashMap;
use std::sync::Arc;

use crate::service::KernelService;

/// `POST /api/v1/webhooks/telegram/{channel_id}`
///
/// Receives Telegram Bot API update POSTs.  Verified via the
/// `X-Telegram-Bot-Api-Secret-Token` header that Telegram sends when a
/// `secret_token` was set in the `setWebhook` call.
#[utoipa::path(
    post,
    path = "/api/v1/webhooks/telegram/{channel_id}",
    tag = "webhooks",
    operation_id = "webhooks_telegram",
    params(("channel_id" = String, Path, description = "Channel ID")),
    request_body(content = serde_json::Value, description = "Telegram Bot API update payload"),
    responses(
        (status = 200, description = "Webhook accepted", body = serde_json::Value),
        (status = 401, description = "Invalid secret token", body = crate::error::ApiErrorBody)
    )
)]
pub async fn telegram_webhook(
    State(svc): State<Arc<dyn KernelService>>,
    Path(channel_id): Path<String>,
    headers: HeaderMap,
    Json(update): Json<agentos_kernel::adapters::telegram::TelegramUpdate>,
) -> StatusCode {
    // Verify the secret token header.
    let secret = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match svc.verify_webhook_secret(&channel_id, secret).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(channel_id = %channel_id, "Telegram webhook: invalid secret token");
            return StatusCode::UNAUTHORIZED;
        }
        Err(_) => return StatusCode::BAD_REQUEST,
    }

    let cid: agentos_types::ChannelInstanceID = match channel_id.parse() {
        Ok(id) => id,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let pinned = match svc.channel_pinned_external_id(&channel_id).await {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    // When a chat_id was configured at connect time, only accept that chat — even
    // if the webhook URL and secret were compromised, other chats cannot drive commands.
    let inbound = match pinned
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(registered_chat_id) => agentos_kernel::adapters::telegram::extract_inbound_message(
            &update,
            registered_chat_id,
            cid,
        ),
        None => {
            let first =
                agentos_kernel::adapters::telegram::extract_inbound_message(&update, "", cid);
            first.or_else(|| {
                let chat_id =
                    agentos_kernel::adapters::telegram::extract_chat_id_from_update(&update)?;
                agentos_kernel::adapters::telegram::extract_inbound_message(&update, &chat_id, cid)
            })
        }
    };

    if let Some(msg) = inbound {
        if let Err(e) = svc.forward_webhook_message(msg).await {
            tracing::error!(error = %e, "Failed to forward Telegram webhook message");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    // Telegram expects 200 OK; any other status triggers retries.
    StatusCode::OK
}

/// `GET /api/v1/webhooks/whatsapp/{channel_id}`
///
/// WhatsApp Cloud API webhook verification handshake. Meta sends
/// `?hub.mode=subscribe&hub.verify_token=<token>&hub.challenge=<n>`; we echo the
/// challenge iff the token matches the channel's configured verify-token
/// (vault `{credential_key}.verify_token`).
pub async fn whatsapp_webhook_verify(
    State(svc): State<Arc<dyn KernelService>>,
    Path(channel_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> axum::response::Response {
    let mode = params.get("hub.mode").map(String::as_str).unwrap_or("");
    let token = params
        .get("hub.verify_token")
        .map(String::as_str)
        .unwrap_or("");
    let challenge = params.get("hub.challenge").cloned().unwrap_or_default();

    let expected = svc.whatsapp_verify_token(&channel_id).await.ok().flatten();
    match expected {
        Some(t) if mode == "subscribe" && !t.is_empty() && t == token => {
            (StatusCode::OK, challenge).into_response()
        }
        _ => {
            tracing::warn!(channel_id = %channel_id, "WhatsApp webhook verify: token mismatch");
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

/// `POST /api/v1/webhooks/whatsapp/{channel_id}`
///
/// Receives WhatsApp Cloud API message webhooks. The raw body is HMAC-verified
/// against the channel's app secret (`X-Hub-Signature-256`); messages are parsed
/// and forwarded to the kernel. Media is resolved + downloaded kernel-side.
pub async fn whatsapp_webhook(
    State(svc): State<Arc<dyn KernelService>>,
    Path(channel_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match svc
        .verify_whatsapp_signature(&channel_id, &body, signature)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(channel_id = %channel_id, "WhatsApp webhook: invalid signature");
            return StatusCode::UNAUTHORIZED;
        }
        Err(_) => return StatusCode::BAD_REQUEST,
    }

    let cid: agentos_types::ChannelInstanceID = match channel_id.parse() {
        Ok(id) => id,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    for msg in agentos_kernel::adapters::whatsapp::parse_whatsapp_inbound(&payload, cid) {
        if let Err(e) = svc.forward_webhook_message(msg).await {
            tracing::error!(error = %e, "Failed to forward WhatsApp webhook message");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    StatusCode::OK
}
