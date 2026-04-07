use crate::notification_router::{DeliveryAdapter, DeliveryError, InboundMessage};
use agentos_types::{
    ChannelInstanceID, DeliveryChannel, NotificationPriority, UserMessage, UserMessageKind,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};

/// Notification sent once when a Telegram chat_id is auto-discovered.
pub struct ChatDiscovered {
    pub channel_instance_id: ChannelInstanceID,
    pub chat_id: String,
}

/// Telegram Bot API delivery and inbound adapter.
///
/// Outbound: sends formatted messages via `sendMessage` (with optional inline
/// keyboard for `Question` messages).
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
    bot_token: String,
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
            bot_token,
            chat_id: Arc::new(RwLock::new(chat_id)),
            channel_instance_id,
            client,
            on_chat_discovered: Arc::new(std::sync::Mutex::new(on_chat_discovered)),
            webhook_mode: false,
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.bot_token)
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
        let resp = self
            .client
            .post(self.api_url("setWebhook"))
            .json(&serde_json::json!({
                "url": webhook_url,
                "secret_token": secret_token,
                "allowed_updates": ["message", "callback_query"],
            }))
            .send()
            .await
            .map_err(|_| DeliveryError("setWebhook request failed".into()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(DeliveryError(format!("setWebhook failed: {body}")));
        }

        let body: serde_json::Value = serde_json::from_str(
            &resp
                .text()
                .await
                .unwrap_or_else(|_| r#"{"ok":false}"#.into()),
        )
        .unwrap_or_default();
        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let desc = body
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(DeliveryError(format!("setWebhook rejected: {desc}")));
        }
        Ok(())
    }

    /// Call Telegram `deleteWebhook` to unregister the webhook and revert to polling.
    pub async fn delete_webhook(&self) -> Result<(), DeliveryError> {
        let resp = self
            .client
            .post(self.api_url("deleteWebhook"))
            .send()
            .await
            .map_err(|_| DeliveryError("deleteWebhook request failed".into()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(DeliveryError(format!("deleteWebhook failed: {body}")));
        }
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

        let text = format_telegram_message(msg);
        let reply_markup = build_inline_keyboard(msg);

        let mut payload = serde_json::json!({
            "chat_id": &*chat_id,
            "text": text,
            "parse_mode": "MarkdownV2",
        });
        if !reply_markup.is_null() {
            payload["reply_markup"] = reply_markup;
        }

        let resp = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
            // Suppress the error value — it contains the full API URL including the bot token.
            .map_err(|_| DeliveryError("Telegram sendMessage request failed".into()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(DeliveryError(format!(
                "Telegram sendMessage HTTP {status}: {body}"
            )));
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

    async fn start_listening(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Result<tokio::task::JoinHandle<()>, DeliveryError> {
        let token = self.bot_token.clone();
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
                match resp.json::<TelegramUpdatesResponse>().await {
                    Ok(updates) if updates.ok => {
                        backoff_secs = 5; // reset only on ok=true
                        for update in updates.result {
                            offset = update.update_id + 1;

                            // Auto-discover chat_id from the first inbound message.
                            let current = chat_id.read().await.clone();
                            if current.is_empty() {
                                if let Some(discovered) = extract_chat_id_from_update(&update) {
                                    tracing::info!(
                                        %discovered,
                                        channel = %channel_instance_id,
                                        "Telegram chat_id auto-discovered"
                                    );
                                    *chat_id.write().await = discovered.clone();
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
                                match client
                                    .post(&ack_url)
                                    .json(&serde_json::json!({
                                        "callback_query_id": cq.id
                                    }))
                                    .send()
                                    .await
                                {
                                    Ok(resp) if !resp.status().is_success() => {
                                        tracing::warn!(
                                            "answerCallbackQuery HTTP {}",
                                            resp.status()
                                        );
                                    }
                                    Err(_) => {
                                        tracing::warn!(
                                            "answerCallbackQuery request failed (details redacted)"
                                        );
                                    }
                                    _ => {}
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

/// Format a `UserMessage` for Telegram (MarkdownV2 escaping for special chars).
fn format_telegram_message(msg: &UserMessage) -> String {
    let icon = match msg.priority {
        NotificationPriority::Critical => "\u{1f6a8}",
        NotificationPriority::Urgent => "\u{26a0}\u{fe0f}",
        NotificationPriority::Warning => "\u{1f536}",
        NotificationPriority::Info => "\u{2139}\u{fe0f}",
    };

    let subject = escape_markdown_v2(&msg.subject);
    // Escape before truncating to avoid splitting an escape sequence at the boundary.
    let body = escape_markdown_v2(&msg.body);
    let body: String = body.chars().take(2000).collect();

    if body.is_empty() {
        format!("{icon} *AgentOS* \u{2014} {subject}")
    } else {
        format!("{icon} *AgentOS* \u{2014} {subject}\n\n{body}")
    }
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
                .map(|row| {
                    row.iter()
                        .map(|opt| {
                            // Telegram limits callback_data to 64 bytes; truncate by char boundary.
                            let cb: String = opt.chars().take(64).collect();
                            serde_json::json!({
                                "text": opt,
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

/// Escape special characters for Telegram MarkdownV2.
///
/// `\` must be escaped first (before adding `\` escapes for other chars),
/// otherwise the escape characters themselves would be double-escaped.
fn escape_markdown_v2(text: &str) -> String {
    const SPECIAL: &[char] = &[
        '\\', '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.',
        '!',
    ];
    let mut out = String::with_capacity(text.len() + 16);
    for ch in text.chars() {
        if SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
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
