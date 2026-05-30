use crate::notification_router::{DeliveryAdapter, DeliveryError, InboundMessage};
use agentos_types::{
    AttachmentKind, ChannelInstanceID, DeliveryChannel, NotificationPriority, NotificationSource,
    UserMessage, UserMessageKind,
};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
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
        // Clone the discovered chat_id and release the read lock immediately —
        // holding it across the awaited HTTP sends (with up to ~1h of bounded
        // 429 backoff) would block inbound chat_id discovery writes.
        let chat_id: String = {
            let guard = self.chat_id.read().await;
            if guard.is_empty() {
                return Err(DeliveryError(
                    "Telegram chat_id not yet discovered — send /start to the bot first".into(),
                ));
            }
            guard.clone()
        };

        let reply_to = msg
            .reply_to_external_id
            .as_ref()
            .and_then(|s| s.parse::<i64>().ok());

        // Media attachment: Telegram fetches the URL itself via sendPhoto /
        // sendDocument. The text body (if any) is still sent as a normal message
        // by the loop below, so a long body is never truncated into a caption.
        if let Some(att) = &msg.attachment {
            let (method, field) = match att.kind {
                AttachmentKind::Image => ("sendPhoto", "photo"),
                AttachmentKind::Document => ("sendDocument", "document"),
            };
            let mut payload = json!({ "chat_id": &chat_id });
            payload[field] = json!(att.url);
            // Telegram caps captions at 1024 chars. Render markdown → HTML, but
            // only use it when the rendered form still fits; HTML entity/tag
            // expansion can push a 1024-char source over the limit, so fall back
            // to the plain (un-rendered) caption rather than risk MESSAGE_TOO_LONG.
            if let Some(cap) = &att.caption {
                let plain_cap: String = cap.chars().take(1024).collect();
                let html_cap =
                    agentos_channels::telegram_format::markdown_to_telegram_html(&plain_cap);
                if html_cap.chars().count() <= 1024 {
                    payload["caption"] = json!(html_cap);
                    payload["parse_mode"] = json!("HTML");
                } else {
                    payload["caption"] = json!(plain_cap);
                }
            }
            if let Some(rid) = reply_to {
                payload["reply_to_message_id"] = json!(rid);
            }
            let media_url = self.api_url(method);
            if let Err(e) = telegram_post_json_with_retry(&self.client, &media_url, &payload).await
            {
                let es = format!("{e}");
                // Only retry as plain text on an actual entity-parse failure; a
                // generic 400 (bad/unreachable URL, unsupported type) would just
                // fail again, so surface it directly.
                if payload.get("parse_mode").is_some() && es.contains("can't parse entities") {
                    tracing::warn!(error = %e, "Telegram media caption HTML parse failed; resending caption as plain");
                    let plain_cap: String = att
                        .caption
                        .as_deref()
                        .unwrap_or_default()
                        .chars()
                        .take(1024)
                        .collect();
                    payload["caption"] = json!(plain_cap);
                    if let Some(obj) = payload.as_object_mut() {
                        obj.remove("parse_mode");
                    }
                    telegram_post_json_with_retry(&self.client, &media_url, &payload).await?;
                } else {
                    return Err(e);
                }
            }
        }

        // Skip the synthetic notification banner when this is an attachment-only
        // message with no body — the media speaks for itself.
        let plain = if msg.attachment.is_some() && msg.body.trim().is_empty() {
            String::new()
        } else {
            format_telegram_plain(msg)
        };
        let reply_markup = build_inline_keyboard(msg);

        let url = self.api_url("sendMessage");
        let has_markup = !reply_markup.is_null();

        // Chunk the plain source first (Telegram's char limit is on the rendered
        // text), then convert each chunk independently. Converting then chunking
        // could split an HTML tag mid-attribute. The HTML-escaped output is at
        // most 5x longer than the input (`>` → `&gt;`), so we use a smaller
        // input cap to leave room. Inline keyboards (Question payloads) must
        // attach to a single message — never split.
        let plain_chunks: Vec<String> = if has_markup {
            vec![plain.chars().take(TELEGRAM_MAX_MESSAGE_CHARS).collect()]
        } else {
            // ~3000 chars of input fit safely under 4096 chars after expansion.
            chunk_telegram_text(&plain, 3000)
        };

        let chunks: Vec<(String, String)> = plain_chunks
            .into_iter()
            .map(|p| {
                let h = agentos_channels::telegram_format::markdown_to_telegram_html(&p);
                (h, p)
            })
            .collect();

        let n = chunks.len();
        for (i, (html_chunk, plain_chunk)) in chunks.iter().enumerate() {
            if html_chunk.is_empty() && plain_chunk.is_empty() {
                continue;
            }
            let is_last = i + 1 == n;
            let mut payload = json!({
                "chat_id": &*chat_id,
                "text": html_chunk,
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
            });
            if is_last {
                if let Some(rid) = reply_to {
                    payload["reply_to_message_id"] = json!(rid);
                }
                if has_markup {
                    payload["reply_markup"] = reply_markup.clone();
                }
            }

            match telegram_post_json_with_retry(&self.client, &url, &payload).await {
                Ok(_) => {}
                Err(e) => {
                    // Parse failure → resend the same segment as plain text.
                    let es = format!("{e}");
                    if es.contains("can't parse entities") || es.contains("HTTP 400") {
                        tracing::warn!(error = %e, "Telegram HTML parse failed; resending as plain");
                        let mut fb = json!({
                            "chat_id": &*chat_id,
                            "text": plain_chunk,
                            "disable_web_page_preview": true,
                        });
                        if is_last {
                            if let Some(rid) = reply_to {
                                fb["reply_to_message_id"] = json!(rid);
                            }
                            if has_markup {
                                fb["reply_markup"] = reply_markup.clone();
                            }
                        }
                        let _ = telegram_post_json_with_retry(&self.client, &url, &fb).await?;
                    } else {
                        return Err(e);
                    }
                }
            }
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

/// Maximum bytes downloaded for an inbound media file. Bounds memory and matches
/// the spirit of the web upload cap; Telegram's Bot API getFile is limited to
/// 20 MiB regardless.
pub const TELEGRAM_MAX_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;

/// Detect a content type from leading magic bytes. Returns a best-effort MIME or
/// `application/octet-stream` when unrecognized.
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        // RIFF....WEBP
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [b'%', b'P', b'D', b'F', ..] => "application/pdf",
        // OGG container (Telegram voice notes are OGG/Opus)
        [b'O', b'g', b'g', b'S', ..] => "audio/ogg",
        [b'I', b'D', b'3', ..] => "audio/mpeg",
        // ISO-BMFF (mp4): bytes 4..8 == "ftyp"
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// File extension for a detected MIME (used to name stored files).
pub fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "audio/ogg" => "ogg",
        "audio/mpeg" => "mp3",
        "video/mp4" => "mp4",
        _ => "bin",
    }
}

#[derive(Debug, Deserialize)]
struct GetFileResponse {
    ok: bool,
    #[serde(default)]
    result: Option<GetFileResult>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetFileResult {
    file_path: Option<String>,
    #[serde(default)]
    file_size: Option<i64>,
}

/// Download an inbound Telegram file by `file_id`.
///
/// Calls `getFile` to resolve the temporary file path, enforces `max_bytes`
/// against the reported size, then fetches the bytes from the Bot API file
/// endpoint and sniffs the MIME from magic bytes. The download URL is always
/// `api.telegram.org` derived from Telegram's own `file_path`, so it is not
/// attacker-controllable (no SSRF surface). Returns `(bytes, detected_mime)`.
pub async fn download_telegram_file(
    client: &reqwest::Client,
    token: &str,
    file_id: &str,
    max_bytes: u64,
) -> Result<(Vec<u8>, String), DeliveryError> {
    let get_file_url = format!("https://api.telegram.org/bot{token}/getFile");
    let resp = client
        .post(&get_file_url)
        .json(&json!({ "file_id": file_id }))
        .send()
        .await
        .map_err(|_| DeliveryError("Telegram getFile request failed (details redacted)".into()))?;
    let parsed: GetFileResponse = resp
        .json()
        .await
        .map_err(|_| DeliveryError("Telegram getFile parse failed".into()))?;
    if !parsed.ok {
        return Err(DeliveryError(format!(
            "Telegram getFile error: {}",
            parsed.description.as_deref().unwrap_or("unknown")
        )));
    }
    let result = parsed
        .result
        .ok_or_else(|| DeliveryError("Telegram getFile returned no result".into()))?;
    let file_path = result
        .file_path
        .ok_or_else(|| DeliveryError("Telegram getFile returned no file_path".into()))?;
    if let Some(sz) = result.file_size {
        if sz as u64 > max_bytes {
            return Err(DeliveryError(format!(
                "Telegram media too large: {sz} bytes (cap {max_bytes})"
            )));
        }
    }

    // Telegram-derived path; not attacker-controllable. NOTE: `token` is
    // embedded in this URL — reqwest errors (whose Display includes the URL)
    // must never be logged verbatim; all error arms here are redacted.
    let dl_url = format!("https://api.telegram.org/file/bot{token}/{file_path}");
    let dl =
        client.get(&dl_url).send().await.map_err(|_| {
            DeliveryError("Telegram file download failed (details redacted)".into())
        })?;
    if !dl.status().is_success() {
        return Err(DeliveryError(format!(
            "Telegram file download HTTP {}",
            dl.status().as_u16()
        )));
    }
    // Pre-check Content-Length when present (covers the case where getFile
    // omitted file_size), then stream the body and enforce the cap
    // incrementally so memory is bounded even without a declared length.
    if let Some(len) = dl.content_length() {
        if len > max_bytes {
            return Err(DeliveryError(format!(
                "Telegram media too large: {len} bytes (cap {max_bytes})"
            )));
        }
    }
    let mut stream = dl.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| DeliveryError("Telegram file body read failed".into()))?;
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(DeliveryError(format!(
                "Telegram media exceeded cap during download (> {max_bytes} bytes)"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    let mime = sniff_mime(&buf).to_string();
    Ok((buf, mime))
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
            // Telegram puts the user's typed text in `text` for plain messages
            // and in `caption` when media is attached. Compose both plus a note
            // describing any attachment so media messages are no longer dropped.
            let mut parts: Vec<String> = Vec::new();
            if let Some(t) = msg.text.as_deref().filter(|s| !s.is_empty()) {
                parts.push(t.to_string());
            }
            if let Some(c) = msg.caption.as_deref().filter(|s| !s.is_empty()) {
                parts.push(c.to_string());
            }
            if let Some(media) = telegram_media_ref(msg) {
                parts.push(telegram_media_note(&media));
            }
            let text = parts.join("\n");
            if !text.is_empty() {
                return Some(InboundMessage {
                    channel: DeliveryChannel::custom(DeliveryChannel::TELEGRAM),
                    channel_instance_id,
                    external_sender_id: registered_chat_id.to_string(),
                    text,
                    reply_to_notification_id: None,
                    received_at: Utc::now(),
                    raw: serde_json::to_value(msg).unwrap_or_default(),
                    media_file_ids: Vec::new(),
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
                    media_file_ids: Vec::new(),
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
    /// Caption accompanying a media message (photo/document/etc.). Telegram puts
    /// the user's typed text here — NOT in `text` — when media is attached.
    #[serde(default)]
    pub caption: Option<String>,
    /// Photo sizes (ascending). Largest is the best quality.
    #[serde(default)]
    pub photo: Option<Vec<TelegramPhotoSize>>,
    #[serde(default)]
    pub document: Option<TelegramDocument>,
    #[serde(default)]
    pub voice: Option<TelegramVoice>,
    #[serde(default)]
    pub audio: Option<TelegramAudio>,
    #[serde(default)]
    pub video: Option<TelegramVideo>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TelegramPhotoSize {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TelegramDocument {
    pub file_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TelegramVoice {
    pub file_id: String,
    #[serde(default)]
    pub duration: i64,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TelegramAudio {
    pub file_id: String,
    #[serde(default)]
    pub duration: i64,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct TelegramVideo {
    pub file_id: String,
    #[serde(default)]
    pub duration: i64,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<i64>,
}

/// A description of a media attachment found on an inbound Telegram message,
/// plus the `file_id` needed to download it later (kernel-side, where the bot
/// token and storage sink live).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramMediaRef {
    pub file_id: String,
    /// Human label for the agent (e.g. "photo", "voice message").
    pub kind_label: String,
    /// Optional original filename (documents/audio).
    pub filename: Option<String>,
}

/// Identify the primary media attachment on a Telegram message, if any.
/// For photos, the largest size (last in the ascending array) is chosen.
pub fn telegram_media_ref(msg: &TelegramMessage) -> Option<TelegramMediaRef> {
    if let Some(photos) = &msg.photo {
        if let Some(largest) = photos.last() {
            return Some(TelegramMediaRef {
                file_id: largest.file_id.clone(),
                kind_label: "photo".to_string(),
                filename: None,
            });
        }
    }
    if let Some(doc) = &msg.document {
        return Some(TelegramMediaRef {
            file_id: doc.file_id.clone(),
            kind_label: "document".to_string(),
            filename: doc.file_name.clone(),
        });
    }
    if let Some(v) = &msg.voice {
        return Some(TelegramMediaRef {
            file_id: v.file_id.clone(),
            kind_label: "voice message".to_string(),
            filename: None,
        });
    }
    if let Some(a) = &msg.audio {
        return Some(TelegramMediaRef {
            file_id: a.file_id.clone(),
            kind_label: "audio".to_string(),
            filename: a.file_name.clone(),
        });
    }
    if let Some(v) = &msg.video {
        return Some(TelegramMediaRef {
            file_id: v.file_id.clone(),
            kind_label: "video".to_string(),
            filename: None,
        });
    }
    None
}

/// A short note appended to inbound text so the agent knows media arrived and
/// does not hallucinate its contents. Replaced by real content (transcription /
/// vision / stored file_id) once those phases land.
fn telegram_media_note(media: &TelegramMediaRef) -> String {
    match media.kind_label.as_str() {
        "voice message" | "audio" => format!(
            "[The user sent a {} — a transcript follows below if transcription is enabled; otherwise ask them to type the content.]",
            media.kind_label
        ),
        "photo" => "[The user sent a photo.]".to_string(),
        "video" => "[The user sent a video — video understanding is not available, so you cannot watch it. Ask them to describe it if needed.]".to_string(),
        _ => {
            let name = media
                .filename
                .as_deref()
                .map(|n| format!(": {n}"))
                .unwrap_or_default();
            format!(
                "[The user sent a {}{} — file contents are not yet retrievable. Ask them to paste the relevant text if needed.]",
                media.kind_label, name
            )
        }
    }
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

    fn msg_from(value: serde_json::Value) -> TelegramMessage {
        serde_json::from_value(value).expect("valid TelegramMessage")
    }

    #[test]
    fn photo_message_picks_largest_file_id() {
        let m = msg_from(json!({
            "message_id": 1,
            "chat": {"id": 42},
            "photo": [
                {"file_id": "small", "file_size": 100},
                {"file_id": "large", "file_size": 9000}
            ]
        }));
        let media = telegram_media_ref(&m).expect("media");
        assert_eq!(media.file_id, "large");
        assert_eq!(media.kind_label, "photo");
    }

    #[test]
    fn document_message_keeps_filename() {
        let m = msg_from(json!({
            "message_id": 1,
            "chat": {"id": 42},
            "document": {"file_id": "doc1", "file_name": "report.pdf", "mime_type": "application/pdf"}
        }));
        let media = telegram_media_ref(&m).expect("media");
        assert_eq!(media.file_id, "doc1");
        assert_eq!(media.filename.as_deref(), Some("report.pdf"));
    }

    #[test]
    fn inbound_caption_is_captured_not_dropped() {
        // A photo with a caption used to be dropped entirely. Now the caption
        // text and a media note both reach the agent.
        let update: TelegramUpdate = serde_json::from_value(json!({
            "update_id": 1,
            "message": {
                "message_id": 5,
                "chat": {"id": 42},
                "caption": "what is wrong here?",
                "photo": [{"file_id": "p1"}]
            }
        }))
        .unwrap();
        let inbound =
            extract_inbound_message(&update, "42", ChannelInstanceID::new()).expect("inbound");
        assert!(inbound.text.contains("what is wrong here?"));
        assert!(inbound.text.contains("photo"));
    }

    #[test]
    fn inbound_voice_only_still_produces_message() {
        let update: TelegramUpdate = serde_json::from_value(json!({
            "update_id": 2,
            "message": {
                "message_id": 6,
                "chat": {"id": 42},
                "voice": {"file_id": "v1", "duration": 3}
            }
        }))
        .unwrap();
        let inbound =
            extract_inbound_message(&update, "42", ChannelInstanceID::new()).expect("inbound");
        assert!(inbound.text.contains("voice message"));
    }

    #[test]
    fn inbound_plain_text_unchanged() {
        let update: TelegramUpdate = serde_json::from_value(json!({
            "update_id": 3,
            "message": {"message_id": 7, "chat": {"id": 42}, "text": "hello"}
        }))
        .unwrap();
        let inbound =
            extract_inbound_message(&update, "42", ChannelInstanceID::new()).expect("inbound");
        assert_eq!(inbound.text, "hello");
    }
}
