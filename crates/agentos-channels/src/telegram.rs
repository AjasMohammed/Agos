use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use zeroize::Zeroizing;

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    #[allow(dead_code)]
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    #[allow(dead_code)]
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
            client: reqwest::Client::new(),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!(
            "https://api.telegram.org/bot{}/{}",
            self.bot_token.as_str(),
            method
        )
    }
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
            max_message_length: 4096,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let text = msg.content.as_text();
        let resp = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": text,
                "parse_mode": "Markdown"
            }))
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "telegram".to_string(),
                reason: e.to_string(),
            })?;

        if !resp.status().is_success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "telegram".to_string(),
                reason: format!("Telegram API error: {}", resp.status()),
            });
        }

        Ok(DeliveryReceipt {
            message_id: uuid::Uuid::new_v4().to_string(),
            delivered_at: chrono::Utc::now(),
        })
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
                    if let Ok(body) = r.json::<serde_json::Value>().await {
                        if let Some(updates) = body["result"].as_array() {
                            for update in updates {
                                if let Ok(u) =
                                    serde_json::from_value::<TelegramUpdate>(update.clone())
                                {
                                    offset = u.update_id + 1;
                                    if let Some(tg_msg) = u.message {
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
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    // Intentionally omit the error URL to avoid logging the bot token.
                    let kind = if e.is_timeout() {
                        "timeout"
                    } else if e.is_connect() {
                        "connection refused"
                    } else {
                        "network error"
                    };
                    warn!("Telegram poll error ({})", kind);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        match self.client.get(self.api_url("getMe")).send().await {
            Ok(r) if r.status().is_success() => ChannelHealth::Connected,
            Ok(r) => ChannelHealth::Degraded(format!("status {}", r.status())),
            // Intentionally omit e.to_string() to avoid logging the bot token via reqwest URL.
            Err(e) => ChannelHealth::Disconnected(if e.is_timeout() {
                "timeout".to_string()
            } else if e.is_connect() {
                "connection refused".to_string()
            } else {
                "network error".to_string()
            }),
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
