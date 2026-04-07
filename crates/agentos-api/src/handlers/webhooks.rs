//! Webhook ingestion endpoints for external channel adapters (Telegram, etc.).
//!
//! These routes are **public** (no API key required) — authentication is handled
//! via adapter-specific mechanisms (e.g. Telegram's `secret_token` header).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use std::sync::Arc;

use crate::service::KernelService;

/// `POST /api/v1/webhooks/telegram/{channel_id}`
///
/// Receives Telegram Bot API update POSTs.  Verified via the
/// `X-Telegram-Bot-Api-Secret-Token` header that Telegram sends when a
/// `secret_token` was set in the `setWebhook` call.
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

    // Extract the chat_id and text from the update.
    let inbound = agentos_kernel::adapters::telegram::extract_inbound_message(&update, "", cid);

    // If chat_id filter is empty string, extract_inbound_message won't match.
    // Use extract_chat_id_from_update to get the actual chat_id and build the message.
    let inbound = inbound.or_else(|| {
        let chat_id = agentos_kernel::adapters::telegram::extract_chat_id_from_update(&update)?;
        agentos_kernel::adapters::telegram::extract_inbound_message(&update, &chat_id, cid)
    });

    if let Some(msg) = inbound {
        if let Err(e) = svc.forward_webhook_message(msg).await {
            tracing::error!(error = %e, "Failed to forward Telegram webhook message");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    // Telegram expects 200 OK; any other status triggers retries.
    StatusCode::OK
}
