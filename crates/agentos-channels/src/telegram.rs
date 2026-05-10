use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_http::{client, HttpProfile};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use zeroize::Zeroizing;

const TELEGRAM_MAX_TEXT: usize = 4096;
const TELEGRAM_POST_ATTEMPTS: u32 = 4;

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
    #[serde(default)]
    callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Deserialize)]
struct TelegramCallbackQuery {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    chat: TelegramChat,
    #[serde(default)]
    from: Option<TelegramUser>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
    first_name: String,
}

pub struct TelegramAdapter {
    bot_token: Zeroizing<String>,
    pub chat_id: String,
    pub instance_id: String,
    client: reqwest::Client,
}

impl TelegramAdapter {
    pub fn new(bot_token: String, chat_id: String, instance_id: String) -> Self {
        Self {
            bot_token: Zeroizing::new(bot_token),
            chat_id,
            instance_id,
            client: client(HttpProfile::Outbound),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!(
            "https://api.telegram.org/bot{}/{}",
            self.bot_token.as_str(),
            method
        )
    }

    /// POST JSON and verify `{"ok":true}`; retries on HTTP 429 / flood `error_code` 429.
    async fn post_telegram(&self, method: &str, body: &Value) -> Result<Value, AgentOSError> {
        let url = self.api_url(method);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let resp = self
                .client
                .post(&url)
                .json(body)
                .send()
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "telegram".to_string(),
                    reason: e.to_string(),
                })?;

            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let wait = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(3600);
                if attempt >= TELEGRAM_POST_ATTEMPTS {
                    return Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "telegram".to_string(),
                        reason: "Telegram rate limit (HTTP 429), giving up".into(),
                    });
                }
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }

            let bytes = resp
                .bytes()
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "telegram".to_string(),
                    reason: e.to_string(),
                })?;
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(json!({"ok": false}));

            if v.get("error_code").and_then(|c| c.as_i64()) == Some(429) {
                let wait = v
                    .get("parameters")
                    .and_then(|p| p.get("retry_after"))
                    .and_then(|x| x.as_u64())
                    .or_else(|| {
                        v.get("parameters")
                            .and_then(|p| p.get("retry_after"))
                            .and_then(|x| x.as_i64())
                            .map(|i| i.max(0) as u64)
                    })
                    .unwrap_or(1)
                    .min(3600);
                if attempt >= TELEGRAM_POST_ATTEMPTS {
                    return Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "telegram".to_string(),
                        reason: format!("Telegram flood control: {}", telegram_err_summary(&v)),
                    });
                }
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }

            if !status.is_success() {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "telegram".to_string(),
                    reason: format!(
                        "Telegram HTTP {}: {}",
                        status.as_u16(),
                        telegram_err_summary(&v)
                    ),
                });
            }

            if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "telegram".to_string(),
                    reason: telegram_err_summary(&v),
                });
            }

            return Ok(v);
        }
    }
}

fn telegram_err_summary(v: &Value) -> String {
    let desc = v
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown error");
    let code = v.get("error_code").and_then(|x| x.as_i64()).unwrap_or(0);
    format!("Telegram API error {code}: {desc}")
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    fn name(&self) -> &str {
        "telegram"
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: false,
            reactions: true,
            media: true,
            rich_formatting: true,
            max_message_length: TELEGRAM_MAX_TEXT,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        // All outbound text is rendered as Telegram HTML so agents can use
        // markdown (`**bold**`, `*italic*`, code, links, fenced blocks). The
        // converter HTML-escapes raw input first so plain text with `<`/`>`/`&`
        // remains safe.
        let raw = msg.content.as_text();
        let raw: String = raw.chars().take(TELEGRAM_MAX_TEXT).collect();
        let html = crate::telegram_format::markdown_to_telegram_html(&raw);
        let chat_id = self.chat_id.clone();
        let policy = crate::retry::RetryPolicy::default();

        crate::retry::with_retry(&policy, "telegram", || async {
            // Try HTML mode first; on Telegram parse errors fall back to plain.
            let html_payload = json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
            });
            let v = match self.post_telegram("sendMessage", &html_payload).await {
                Ok(v) => v,
                Err(e) => {
                    // 400 with description "can't parse entities" → fall back to plain raw.
                    let msg = format!("{e}");
                    if msg.contains("can't parse entities") || msg.contains("HTTP 400") {
                        warn!(error = %e, "Telegram HTML parse failed; resending as plain text");
                        self.post_telegram(
                            "sendMessage",
                            &json!({
                                "chat_id": chat_id,
                                "text": raw,
                                "disable_web_page_preview": true,
                            }),
                        )
                        .await?
                    } else {
                        return Err(e);
                    }
                }
            };

            let ext_id = v
                .get("result")
                .and_then(|r| r.get("message_id"))
                .and_then(|m| m.as_i64())
                .map(|id| id.to_string())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            Ok(DeliveryReceipt {
                message_id: ext_id,
                delivered_at: chrono::Utc::now(),
            })
        })
        .await
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        let mut offset: i64 = 0;
        let client = self.client.clone();
        let url = self.api_url("getUpdates");
        let instance_id = self.instance_id.clone();
        let expected_chat = self.chat_id.clone();
        let mut backoff = Duration::from_secs(5);
        let max_backoff = Duration::from_secs(120);

        loop {
            if cancel.is_cancelled() {
                break;
            }
            let resp = client
                .get(&url)
                .query(&[
                    ("offset", offset.to_string()),
                    ("timeout", "30".to_string()),
                ])
                .send()
                .await;

            match resp {
                Ok(r) => {
                    if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        let wait = r
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|h| h.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(5)
                            .min(300);
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                        }
                        continue;
                    }

                    backoff = Duration::from_secs(5);
                    let body: Value = match r.json().await {
                        Ok(b) => b,
                        Err(e) => {
                            warn!(error = %e, "Telegram getUpdates JSON parse error; retrying");
                            continue;
                        }
                    };

                    if body.get("ok").and_then(|o| o.as_bool()) != Some(true) {
                        if body.get("error_code").and_then(|c| c.as_i64()) == Some(429) {
                            let wait = body
                                .get("parameters")
                                .and_then(|p| p.get("retry_after"))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(5)
                                .min(300);
                            tokio::select! {
                                _ = cancel.cancelled() => break,
                                _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                            }
                            continue;
                        }
                        let jitter_ms = rand::thread_rng().gen_range(0u64..=1000);
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(backoff + Duration::from_millis(jitter_ms)) => {}
                        }
                        backoff = backoff.saturating_mul(2).min(max_backoff);
                        continue;
                    }

                    let Some(updates) = body["result"].as_array() else {
                        continue;
                    };

                    for update in updates {
                        let Ok(u) = serde_json::from_value::<TelegramUpdate>(update.clone()) else {
                            continue;
                        };
                        offset = u.update_id + 1;

                        if let Some(tg_msg) = u.message {
                            if tg_msg.chat.id.to_string() != expected_chat {
                                continue;
                            }
                            if let Some(text) = tg_msg.text {
                                let inbound = InboundMessage {
                                    id: tg_msg.message_id.to_string(),
                                    channel_type: "telegram".to_string(),
                                    channel_instance_id: instance_id.clone(),
                                    sender: ChannelIdentity {
                                        platform_id: tg_msg
                                            .from
                                            .as_ref()
                                            .map(|f| f.id.to_string())
                                            .unwrap_or_default(),
                                        display_name: tg_msg.from.map(|f| f.first_name),
                                    },
                                    content: MessageContent::Text(text),
                                    thread_id: None,
                                    timestamp: chrono::Utc::now(),
                                    raw: update.clone(),
                                };
                                let _ = tx.send(inbound).await;
                            }
                            continue;
                        }

                        if let Some(cq) = u.callback_query {
                            let Some(m) = cq.message else {
                                continue;
                            };
                            if m.chat.id.to_string() != expected_chat {
                                continue;
                            }
                            let data = cq.data.clone().unwrap_or_default();
                            if data.is_empty() {
                                continue;
                            }
                            let inbound = InboundMessage {
                                id: m.message_id.to_string(),
                                channel_type: "telegram".to_string(),
                                channel_instance_id: instance_id.clone(),
                                sender: ChannelIdentity {
                                    platform_id: m
                                        .from
                                        .as_ref()
                                        .map(|f| f.id.to_string())
                                        .unwrap_or_default(),
                                    display_name: m.from.map(|f| f.first_name),
                                },
                                content: MessageContent::Text(data),
                                thread_id: None,
                                timestamp: chrono::Utc::now(),
                                raw: update.clone(),
                            };
                            let _ = tx.send(inbound).await;
                        }
                    }
                }
                Err(e) => {
                    let kind = if e.is_timeout() {
                        "timeout"
                    } else if e.is_connect() {
                        "connection refused"
                    } else {
                        "network error"
                    };
                    let jitter_ms = rand::thread_rng().gen_range(0u64..=1000);
                    warn!(
                        error = kind,
                        backoff_ms = backoff.as_millis() as u64 + jitter_ms,
                        "Telegram poll error, backing off"
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(backoff + Duration::from_millis(jitter_ms)) => {}
                    }
                    backoff = backoff.saturating_mul(2).min(max_backoff);
                }
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        match self.post_telegram("getMe", &json!({})).await {
            Ok(_) => ChannelHealth::Connected,
            Err(e) => ChannelHealth::Disconnected(format!("{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_telegram_update() {
        let json = serde_json::json!({
            "update_id": 123,
            "message": {
                "message_id": 456,
                "chat": {"id": 789, "type": "private"},
                "from": {"id": 111, "first_name": "Test", "is_bot": false},
                "text": "Hello agent",
                "date": 1234567890
            }
        });
        let update: TelegramUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(update.update_id, 123);
        let msg = update.message.unwrap();
        assert_eq!(msg.text.unwrap(), "Hello agent");
        assert_eq!(msg.chat.id, 789);
    }

    #[test]
    fn test_message_content_as_text() {
        assert_eq!(MessageContent::Text("hi".into()).as_text(), "hi");
        assert_eq!(
            MessageContent::Markdown("**bold**".into()).as_text(),
            "**bold**"
        );
    }
}
