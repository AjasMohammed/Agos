use crate::notification_router::{DeliveryAdapter, DeliveryError, InboundMessage};
use agentos_types::{
    ChannelInstanceID, DeliveryChannel, NotificationPriority, UserMessage, UserMessageKind,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;

/// Telegram Bot API delivery and inbound adapter.
///
/// Outbound: sends formatted messages via `sendMessage` (with optional inline
/// keyboard for `Question` messages).
/// Inbound: long-polls `getUpdates` in a background task; every message from
/// the registered chat is forwarded to the `InboundRouter` via `mpsc::Sender`.
///
/// Bot tokens are stored in `agentos-vault`; only the credential vault key is
/// kept here.  The actual token is retrieved at startup by the kernel and passed
/// to `new()`.
pub struct TelegramDeliveryAdapter {
    bot_token: String,
    chat_id: String,
    channel_instance_id: ChannelInstanceID,
    client: reqwest::Client,
}

impl TelegramDeliveryAdapter {
    /// Construct the adapter.
    ///
    /// `bot_token` — the Telegram Bot API token (from vault).
    /// `chat_id` — the chat/user ID to deliver to and poll.
    /// `channel_instance_id` — used to tag inbound messages.
    pub fn new(bot_token: String, chat_id: String, channel_instance_id: ChannelInstanceID) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(40))
            .build()
            .unwrap_or_default();
        Self {
            bot_token,
            chat_id,
            channel_instance_id,
            client,
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.bot_token)
    }
}

#[async_trait]
impl DeliveryAdapter for TelegramDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::custom(DeliveryChannel::TELEGRAM)
    }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        let text = format_telegram_message(msg);
        let reply_markup = build_inline_keyboard(msg);

        let mut payload = serde_json::json!({
            "chat_id": self.chat_id,
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
        !self.bot_token.is_empty() && !self.chat_id.is_empty()
    }

    fn adapter_instance_id(&self) -> Option<String> {
        Some(self.channel_instance_id.to_string())
    }

    fn supports_inbound(&self) -> bool {
        true
    }

    async fn start_listening(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Result<tokio::task::JoinHandle<()>, DeliveryError> {
        let token = self.bot_token.clone();
        let chat_id = self.chat_id.clone();
        let channel_instance_id = self.channel_instance_id;
        let client = self.client.clone();

        let handle = tokio::spawn(async move {
            telegram_poll_loop(token, chat_id, channel_instance_id, client, tx).await;
        });
        Ok(handle)
    }
}

/// Long-poll loop: calls `getUpdates` with a 30-second timeout repeatedly.
/// Uses exponential backoff (5s → 80s) on transient failures.
async fn telegram_poll_loop(
    token: String,
    chat_id: String,
    channel_instance_id: ChannelInstanceID,
    client: reqwest::Client,
    tx: mpsc::Sender<InboundMessage>,
) {
    let mut offset: i64 = 0;
    let mut backoff_secs: u64 = 5;
    loop {
        // Build URL without logging it — the token must not appear in log output.
        let url =
            format!("https://api.telegram.org/bot{token}/getUpdates?offset={offset}&timeout=30");
        match client.get(&url).send().await {
            Ok(resp) => {
                backoff_secs = 5; // reset on success
                match resp.json::<TelegramUpdatesResponse>().await {
                    Ok(updates) if updates.ok => {
                        for update in updates.result {
                            offset = update.update_id + 1;
                            let inbound =
                                extract_inbound_message(&update, &chat_id, channel_instance_id);
                            if let Some(msg) = inbound {
                                if tx.send(msg).await.is_err() {
                                    // Router dropped — kernel shutting down.
                                    return;
                                }
                            }
                        }
                    }
                    Ok(_) => {
                        tracing::warn!(
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

/// Extract an `InboundMessage` from a Telegram update, filtering to the registered chat.
fn extract_inbound_message(
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
        NotificationPriority::Critical => "🚨",
        NotificationPriority::Urgent => "⚠️",
        NotificationPriority::Warning => "🔶",
        NotificationPriority::Info => "ℹ️",
    };

    let subject = escape_markdown_v2(&msg.subject);
    // Escape before truncating to avoid splitting an escape sequence at the boundary.
    let body = escape_markdown_v2(&msg.body);
    let body: String = body.chars().take(2000).collect();

    if body.is_empty() {
        format!("{icon} *AgentOS* — {subject}")
    } else {
        format!("{icon} *AgentOS* — {subject}\n\n{body}")
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
struct TelegramUpdatesResponse {
    ok: bool,
    #[serde(default)]
    result: Vec<TelegramUpdate>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
    #[serde(default)]
    callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct TelegramMessage {
    #[allow(dead_code)]
    message_id: i64,
    chat: TelegramChat,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct TelegramCallbackQuery {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    message: Option<TelegramMessage>,
}
