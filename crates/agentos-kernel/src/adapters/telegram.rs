use crate::notification_router::{DeliveryAdapter, DeliveryError, InboundMessage};
use agentos_types::{
    ChannelInstanceID, DeliveryChannel, NotificationPriority, NotificationSource, UserMessage,
    UserMessageKind,
};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use zeroize::Zeroizing;

/// Telegram documents a 4096-character cap on `sendMessage` text (stricter when using entities).
const TELEGRAM_MAX_MESSAGE_CHARS: usize = 4096;
const TELEGRAM_POST_MAX_ATTEMPTS: u32 = 5;

fn telegram_error_summary(v: &Value) -> String {
    let desc = v
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown error");
    let code = v.get("error_code").and_then(|x| x.as_i64()).unwrap_or(0);
    format!("Telegram API error {code}: {desc}")
}

fn retry_after_from_telegram_json(v: &Value) -> u64 {
    v.get("parameters")
        .map(retry_after_from_parameters_object)
        .unwrap_or(1)
}

fn retry_after_from_parameters_object(p: &Value) -> u64 {
    p.get("retry_after")
        .and_then(|x| x.as_u64())
        .or_else(|| {
            p.get("retry_after")
                .and_then(|x| x.as_i64())
                .map(|i| i.max(0) as u64)
        })
        .unwrap_or(1)
        .min(3600)
}

/// POST JSON to the Bot API, sleeping and retrying on HTTP 429 or flood-wait (`error_code` 429).
async fn telegram_post_json_with_retry(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
) -> Result<Value, DeliveryError> {
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let resp =
            client.post(url).json(body).send().await.map_err(|_| {
                DeliveryError("Telegram HTTP request failed (details redacted)".into())
            })?;

        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let hdr_wait = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(1)
                .min(3600);
            if attempt >= TELEGRAM_POST_MAX_ATTEMPTS {
                return Err(DeliveryError(
                    "Telegram rate limit: too many retries (HTTP 429)".into(),
                ));
            }
            tokio::time::sleep(Duration::from_secs(hdr_wait)).await;
            continue;
        }

        let bytes = resp.bytes().await.map_err(|_| {
            DeliveryError("Telegram response body read failed (details redacted)".into())
        })?;
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(json!({"ok": false}));

        if v.get("error_code").and_then(|c| c.as_i64()) == Some(429) {
            let wait = retry_after_from_telegram_json(&v).max(1);
            if attempt >= TELEGRAM_POST_MAX_ATTEMPTS {
                return Err(DeliveryError(format!(
                    "Telegram flood control: {}",
                    telegram_error_summary(&v)
                )));
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
            continue;
        }

        if !status.is_success() {
            return Err(DeliveryError(format!(
                "Telegram HTTP {}: {}",
                status.as_u16(),
                telegram_error_summary(&v)
            )));
        }

        if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            return Err(DeliveryError(telegram_error_summary(&v)));
        }

        return Ok(v);
    }
}

/// Split plain UTF-8 text into chunks that stay within the Bot API length cap.
fn chunk_telegram_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut rest = text.to_string();
    while !rest.is_empty() {
        let piece: String = rest.chars().take(max_chars).collect();
        let n = piece.chars().count();
        rest = rest.chars().skip(n).collect();
        out.push(piece);
    }
    out
}

/// Notification sent once when a Telegram chat_id is auto-discovered.
pub struct ChatDiscovered {
    pub channel_instance_id: ChannelInstanceID,
    pub chat_id: String,
}

/// Telegram Bot API delivery and inbound adapter.
///
/// Outbound: sends formatted messages via `sendMessage` (with optional inline
/// keyboard for `Question` messages). Long bodies are split across multiple
/// `sendMessage` calls (except interactive `Question` payloads, which stay on
/// one message so the inline keyboard remains valid). Plain notifications disable
/// link previews and honor `UserMessage::reply_to_external_id` as Telegram
/// `reply_to_message_id` on the final segment. HTTP 429 / flood `error_code` 429
/// responses trigger bounded backoff using `retry_after` / `Retry-After`.
///
/// Inbound: long-polls `getUpdates` in a background task; every message from
/// the registered chat is forwarded to the `InboundRouter` via `mpsc::Sender`.
///
/// When `chat_id` is empty, the adapter enters **auto-discovery mode**: it polls
/// `getUpdates`, and the first inbound message's chat_id is captured and stored.
/// Outbound delivery is unavailable until discovery completes.
///
/// Bot tokens are stored in `agentos-vault`; only the credential vault key is
/// kept here.  The actual token is retrieved at startup by the kernel and passed
/// to `new()`.
pub struct TelegramDeliveryAdapter {
    bot_token: Zeroizing<String>,
    chat_id: Arc<RwLock<String>>,
    channel_instance_id: ChannelInstanceID,
    client: reqwest::Client,
    /// Fires once when chat_id is auto-discovered (empty string → real id).
    on_chat_discovered: Arc<std::sync::Mutex<Option<mpsc::Sender<ChatDiscovered>>>>,
    /// When true, inbound messages arrive via webhook POST, not long-polling.
    webhook_mode: bool,
}

impl TelegramDeliveryAdapter {
    /// Construct the adapter.
    ///
    /// `bot_token` — the Telegram Bot API token (from vault).
    /// `chat_id` — the chat/user ID to deliver to and poll.  Pass an empty
    ///   string to enable auto-discovery from the first inbound message.
    /// `channel_instance_id` — used to tag inbound messages.
    /// `on_chat_discovered` — optional sender; fired once when auto-discovery
    ///   captures a chat_id.
    pub fn new(
        bot_token: String,
        chat_id: String,
        channel_instance_id: ChannelInstanceID,
        on_chat_discovered: Option<mpsc::Sender<ChatDiscovered>>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(40))
            .build()
            .unwrap_or_default();
        Self {
            bot_token: Zeroizing::new(bot_token),
            chat_id: Arc::new(RwLock::new(chat_id)),
            channel_instance_id,
            client,
            on_chat_discovered: Arc::new(std::sync::Mutex::new(on_chat_discovered)),
            webhook_mode: false,
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!(
            "https://api.telegram.org/bot{}/{method}",
            self.bot_token.as_str()
        )
    }

    /// Enable webhook mode. Must be called before `start_listening`.
    pub fn set_webhook_mode(&mut self) {
        self.webhook_mode = true;
    }

    /// Call Telegram `setWebhook` to register the given URL.
    ///
    /// `secret_token` is set as the `secret_token` parameter so Telegram sends it
    /// back in the `X-Telegram-Bot-Api-Secret-Token` header on every update POST.
    pub async fn register_webhook(
        &self,
        webhook_url: &str,
        secret_token: &str,
    ) -> Result<(), DeliveryError> {
        let _ = telegram_post_json_with_retry(
            &self.client,
            &self.api_url("setWebhook"),
            &json!({
                "url": webhook_url,
                "secret_token": secret_token,
                "allowed_updates": ["message", "callback_query"],
                "drop_pending_updates": true,
            }),
        )
        .await?;
        Ok(())
    }

    /// Call Telegram `deleteWebhook` to unregister the webhook and revert to polling.
    pub async fn delete_webhook(&self) -> Result<(), DeliveryError> {
        let _ = telegram_post_json_with_retry(
            &self.client,
            &self.api_url("deleteWebhook"),
            &json!({ "drop_pending_updates": false }),
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl DeliveryAdapter for TelegramDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::custom(DeliveryChannel::TELEGRAM)
    }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        let chat_id = self.chat_id.read().await;
        if chat_id.is_empty() {
            return Err(DeliveryError(
                "Telegram chat_id not yet discovered — send /start to the bot first".into(),
            ));
        }

        // Plain text only — MarkdownV2 is brittle with arbitrary agent/user content
        // (URLs, punctuation, code) and causes sendMessage 400s with no user-visible copy.
        let text = format_telegram_plain(msg);
        let reply_markup = build_inline_keyboard(msg);
        let reply_to = msg
            .reply_to_external_id
            .as_ref()
            .and_then(|s| s.parse::<i64>().ok());

        let url = self.api_url("sendMessage");
        let has_markup = !reply_markup.is_null();

        // Inline keyboards must attach to a single message; avoid splitting Question payloads.
        let chunks: Vec<String> = if has_markup {
            vec![text.chars().take(TELEGRAM_MAX_MESSAGE_CHARS).collect()]
        } else {
            chunk_telegram_text(&text, TELEGRAM_MAX_MESSAGE_CHARS)
        };

        let n = chunks.len();
        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.is_empty() {
                continue;
            }
            let is_last = i + 1 == n;
            let mut payload = json!({
                "chat_id": &*chat_id,
                "text": chunk,
                "disable_web_page_preview": true,
            });
            // Only the final segment replies to the operator message (Telegram rejects invalid ids).
            if is_last {
                if let Some(rid) = reply_to {
                    payload["reply_to_message_id"] = json!(rid);
                }
                if has_markup {
                    payload["reply_markup"] = reply_markup.clone();
                }
            }

            let _ = telegram_post_json_with_retry(&self.client, &url, &payload).await?;
        }

        Ok(())
    }

    async fn is_available(&self) -> bool {
        !self.bot_token.is_empty() && !self.chat_id.read().await.is_empty()
    }

    fn adapter_instance_id(&self) -> Option<String> {
        Some(self.channel_instance_id.to_string())
    }

    fn supports_inbound(&self) -> bool {
        // In webhook mode, inbound messages arrive via the HTTP endpoint,
        // not via a listener task spawned here.
        !self.webhook_mode
    }

    async fn hydrate_discovered_recipient(
        &self,
        channel_instance_id: &ChannelInstanceID,
        external_id: &str,
    ) -> bool {
        if &self.channel_instance_id != channel_instance_id || external_id.is_empty() {
            return false;
        }
        let mut guard = self.chat_id.write().await;
        if guard.is_empty() {
            *guard = external_id.to_string();
            tracing::info!(
                channel = %channel_instance_id,
                chat_id = %external_id,
                "Telegram chat_id hydrated (webhook discovery path)"
            );
            true
        } else {
            false
        }
    }

    async fn start_listening(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Result<tokio::task::JoinHandle<()>, DeliveryError> {
        let token = self.bot_token.as_str().to_string();
        let chat_id = self.chat_id.clone();
        let channel_instance_id = self.channel_instance_id;
        let client = self.client.clone();
        let on_discovered = self
            .on_chat_discovered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();

        let handle = tokio::spawn(async move {
            telegram_poll_loop(
                token,
                chat_id,
                channel_instance_id,
                client,
                tx,
                on_discovered,
            )
            .await;
        });
        Ok(handle)
    }
}

/// Long-poll loop: calls `getUpdates` with a 30-second timeout repeatedly.
/// Uses exponential backoff (5s → 300s) on transient failures.
///
/// When `chat_id` is empty (auto-discovery mode), the first update's chat_id is
/// captured and stored.  The `on_discovered` sender fires once to notify the
/// kernel so it can persist the discovered chat_id in the channel registry.
async fn telegram_poll_loop(
    token: String,
    chat_id: Arc<RwLock<String>>,
    channel_instance_id: ChannelInstanceID,
    client: reqwest::Client,
    tx: mpsc::Sender<InboundMessage>,
    on_discovered: Option<mpsc::Sender<ChatDiscovered>>,
) {
    let mut offset: i64 = 0;
    let mut backoff_secs: u64 = 5;
    let mut discovery_tx = on_discovered;

    loop {
        // Build URL without logging it — the token must not appear in log output.
        let url =
            format!("https://api.telegram.org/bot{token}/getUpdates?offset={offset}&timeout=30");
        match client.get(&url).send().await {
            Ok(resp) => {
                if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                    let wait = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(5)
                        .min(300);
                    tracing::warn!(
                        wait_secs = wait,
                        "Telegram getUpdates HTTP 429; backing off"
                    );
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }
                match resp.json::<TelegramUpdatesResponse>().await {
                    Ok(updates) if updates.ok => {
                        backoff_secs = 5; // reset only on ok=true
                        for update in updates.result {
                            offset = update.update_id + 1;

                            // Auto-discover chat_id from the first inbound message.
                            // Hold the write lock across the is_empty check and the
                            // write to prevent two concurrent updates racing to set
                            // different chat_ids from the same batch of updates.
                            if let Some(discovered) = extract_chat_id_from_update(&update) {
                                let mut guard = chat_id.write().await;
                                if guard.is_empty() {
                                    tracing::info!(
                                        %discovered,
                                        channel = %channel_instance_id,
                                        "Telegram chat_id auto-discovered"
                                    );
                                    *guard = discovered.clone();
                                    drop(guard);
                                    if let Some(dtx) = discovery_tx.take() {
                                        let _ = dtx
                                            .send(ChatDiscovered {
                                                channel_instance_id,
                                                chat_id: discovered,
                                            })
                                            .await;
                                    }
                                }
                            }

                            // Acknowledge inline keyboard taps so Telegram
                            // dismisses the loading spinner on the button.
                            if let Some(cq) = &update.callback_query {
                                let ack_url = format!(
                                    "https://api.telegram.org/bot{token}/answerCallbackQuery"
                                );
                                if let Err(e) = telegram_post_json_with_retry(
                                    &client,
                                    &ack_url,
                                    &json!({
                                        "callback_query_id": cq.id,
                                        "cache_time": 0,
                                    }),
                                )
                                .await
                                {
                                    tracing::warn!(error = %e, "answerCallbackQuery failed");
                                }
                            }

                            // Re-read chat_id (may have just been set by discovery above).
                            let active_chat_id = chat_id.read().await.clone();
                            let inbound = extract_inbound_message(
                                &update,
                                &active_chat_id,
                                channel_instance_id,
                            );
                            if let Some(msg) = inbound {
                                if tx.send(msg).await.is_err() {
                                    // Router dropped — kernel shutting down.
                                    return;
                                }
                            }
                        }
                    }
                    Ok(err_resp) => {
                        let code = err_resp.error_code.unwrap_or(0);
                        if code == 429 {
                            let wait = err_resp
                                .parameters
                                .as_ref()
                                .map(retry_after_from_parameters_object)
                                .unwrap_or(5)
                                .clamp(1, 300);
                            tracing::warn!(wait_secs = wait, "Telegram getUpdates flood wait");
                            tokio::time::sleep(Duration::from_secs(wait)).await;
                            backoff_secs = 5;
                            continue;
                        }
                        let desc = err_resp
                            .description
                            .as_deref()
                            .unwrap_or("(no description)");
                        tracing::warn!(
                            error_code = code,
                            description = desc,
                            "Telegram getUpdates returned ok=false; retrying in {backoff_secs}s"
                        );
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(300);
                    }
                    Err(_) => {
                        tracing::warn!(
                            "Telegram getUpdates parse error; retrying in {backoff_secs}s"
                        );
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(300);
                    }
                }
            }
            Err(_) => {
                // Suppress the error value — it contains the full API URL with the bot token.
                tracing::warn!("Telegram long-poll request failed (details redacted); retrying in {backoff_secs}s");
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(300);
            }
        }
    }
}

/// Pull the chat_id from the first available location in a Telegram update.
pub fn extract_chat_id_from_update(update: &TelegramUpdate) -> Option<String> {
    if let Some(msg) = &update.message {
        return Some(msg.chat.id.to_string());
    }
    if let Some(cq) = &update.callback_query {
        if let Some(m) = &cq.message {
            return Some(m.chat.id.to_string());
        }
    }
    None
}

/// Extract an `InboundMessage` from a Telegram update, filtering to the registered chat.
pub fn extract_inbound_message(
    update: &TelegramUpdate,
    registered_chat_id: &str,
    channel_instance_id: ChannelInstanceID,
) -> Option<InboundMessage> {
    // Regular message
    if let Some(msg) = &update.message {
        if msg.chat.id.to_string() == registered_chat_id {
            let text = msg.text.clone().unwrap_or_default();
            if !text.is_empty() {
                return Some(InboundMessage {
                    channel: DeliveryChannel::custom(DeliveryChannel::TELEGRAM),
                    channel_instance_id,
                    external_sender_id: registered_chat_id.to_string(),
                    text,
                    reply_to_notification_id: None,
                    received_at: Utc::now(),
                    raw: serde_json::to_value(msg).unwrap_or_default(),
                });
            }
        }
    }
    // Inline keyboard button tap (callback_query)
    if let Some(cq) = &update.callback_query {
        if cq
            .message
            .as_ref()
            .map(|m| m.chat.id.to_string())
            .as_deref()
            == Some(registered_chat_id)
        {
            let data = cq.data.clone().unwrap_or_default();
            if !data.is_empty() {
                return Some(InboundMessage {
                    channel: DeliveryChannel::custom(DeliveryChannel::TELEGRAM),
                    channel_instance_id,
                    external_sender_id: registered_chat_id.to_string(),
                    text: data,
                    reply_to_notification_id: None,
                    received_at: Utc::now(),
                    raw: serde_json::to_value(cq).unwrap_or_default(),
                });
            }
        }
    }
    None
}

/// Format a `UserMessage` for Telegram as plain UTF-8 (no parse_mode).
///
/// Keeps under Telegram's 4096 code-point limit for `sendMessage` text.
/// Agent chat replies (from `NotificationSource::Agent`, subject wrapped in `[…]`)
/// are sent as raw body text without the notification banner.
fn format_telegram_plain(msg: &UserMessage) -> String {
    let is_agent_chat_reply = matches!(msg.from, NotificationSource::Agent(_))
        && msg.subject.starts_with('[')
        && msg.subject.ends_with(']');

    if is_agent_chat_reply {
        return msg.body.chars().take(4090).collect();
    }

    let icon = match msg.priority {
        NotificationPriority::Critical => "\u{1f6a8}",
        NotificationPriority::Urgent => "\u{26a0}\u{fe0f}",
        NotificationPriority::Warning => "\u{1f536}",
        NotificationPriority::Info => "\u{2139}\u{fe0f}",
    };

    let subject: String = msg.subject.chars().take(400).collect();
    let body: String = msg.body.chars().take(3600).collect();

    let out = if body.trim().is_empty() {
        format!("{icon} AgentOS — {subject}")
    } else {
        format!("{icon} AgentOS — {subject}\n\n{body}")
    };
    out.chars().take(4090).collect()
}

/// Truncate `s` to at most `max_bytes` UTF-8 bytes without splitting a code point.
fn truncate_to_bytes(s: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(max_bytes.min(s.len()));
    for c in s.chars() {
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

/// Build an inline keyboard for `Question` messages with defined options.
fn build_inline_keyboard(msg: &UserMessage) -> serde_json::Value {
    if let UserMessageKind::Question {
        options: Some(opts),
        ..
    } = &msg.kind
    {
        if !opts.is_empty() {
            let buttons: Vec<Vec<serde_json::Value>> = opts
                .chunks(2)
                .enumerate()
                .map(|(chunk_idx, row)| {
                    row.iter()
                        .enumerate()
                        .map(|(item_idx, opt)| {
                            // Telegram: callback_data ≤ 64 bytes, button text ≤ 64 chars.
                            // Fall back to an index key when truncation produces an empty
                            // string (e.g. the option begins with a 4-byte emoji).
                            let btn_idx = chunk_idx * 2 + item_idx;
                            let cb = {
                                let t = truncate_to_bytes(opt, 64);
                                if t.is_empty() {
                                    format!("opt:{btn_idx}")
                                } else {
                                    t
                                }
                            };
                            let label: String = opt.chars().take(64).collect();
                            serde_json::json!({
                                "text": label,
                                "callback_data": cb,
                            })
                        })
                        .collect()
                })
                .collect();
            return serde_json::json!({ "inline_keyboard": buttons });
        }
    }
    serde_json::Value::Null
}

// ── Telegram API response types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TelegramUpdatesResponse {
    ok: bool,
    #[serde(default)]
    result: Vec<TelegramUpdate>,
    /// Telegram error code (present when `ok` is false).
    #[serde(default)]
    error_code: Option<i32>,
    /// Human-readable error description (present when `ok` is false).
    #[serde(default)]
    description: Option<String>,
    /// Optional `parameters` object (e.g. `retry_after` on flood control).
    #[serde(default)]
    parameters: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TelegramMessage>,
    #[serde(default)]
    pub callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TelegramMessage {
    #[allow(dead_code)]
    pub message_id: i64,
    pub chat: TelegramChat,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TelegramChat {
    pub id: i64,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TelegramCallbackQuery {
    pub id: String,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub message: Option<TelegramMessage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_telegram_text_splits() {
        let s = "a".repeat(9000);
        let parts = chunk_telegram_text(&s, 4096);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 4096);
        assert_eq!(parts[1].len(), 4096);
        assert_eq!(parts[2].len(), 808);
    }

    #[test]
    fn retry_after_from_parameters_object_reads_int() {
        let v = json!({"retry_after": 17});
        assert_eq!(retry_after_from_parameters_object(&v), 17);
    }
}
