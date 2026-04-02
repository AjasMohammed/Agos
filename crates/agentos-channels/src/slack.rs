use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use zeroize::Zeroizing;

pub struct SlackAdapter {
    bot_token: Zeroizing<String>,
    pub channel_id: String,
    pub instance_id: String,
    client: reqwest::Client,
}

impl SlackAdapter {
    pub fn new(bot_token: String, channel_id: String, instance_id: String) -> Self {
        Self {
            bot_token: Zeroizing::new(bot_token),
            channel_id,
            instance_id,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl ChannelAdapter for SlackAdapter {
    fn name(&self) -> &str {
        "slack"
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: true,
            reactions: true,
            media: true,
            rich_formatting: true,
            max_message_length: 40000,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let text = msg.content.as_text();
        let resp = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(self.bot_token.as_str())
            .json(&serde_json::json!({
                "channel": self.channel_id,
                "text": text
            }))
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "slack".to_string(),
                reason: e.to_string(),
            })?;

        let body: serde_json::Value =
            resp.json()
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "slack".to_string(),
                    reason: e.to_string(),
                })?;

        if body["ok"].as_bool() != Some(true) {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "slack".to_string(),
                reason: format!(
                    "Slack error: {}",
                    body["error"].as_str().unwrap_or("unknown")
                ),
            });
        }

        Ok(DeliveryReceipt {
            message_id: body["ts"].as_str().unwrap_or("").to_string(),
            delivered_at: chrono::Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        // Polling-based approach: periodically fetch new messages.
        // In production, use Socket Mode with an app-level token.
        let mut last_ts = chrono::Utc::now().timestamp().to_string();
        let client = self.client.clone();
        let token = self.bot_token.clone();
        let channel = self.channel_id.clone();
        let instance_id = self.instance_id.clone();

        loop {
            if cancel.is_cancelled() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            let resp = client
                .get("https://slack.com/api/conversations.history")
                .bearer_auth(token.as_str())
                .query(&[
                    ("channel", channel.as_str()),
                    ("oldest", last_ts.as_str()),
                    ("limit", "10"),
                ])
                .send()
                .await;

            match resp {
                Ok(r) => {
                    if let Ok(body) = r.json::<serde_json::Value>().await {
                        if let Some(messages) = body["messages"].as_array() {
                            for slack_msg in messages.iter().rev() {
                                let ts = slack_msg["ts"].as_str().unwrap_or("").to_string();
                                if ts > last_ts {
                                    last_ts = ts.clone();
                                    if let Some(text) = slack_msg["text"].as_str() {
                                        if !text.is_empty() {
                                            let inbound = InboundMessage {
                                                id: ts,
                                                channel_type: "slack".to_string(),
                                                channel_instance_id: instance_id.clone(),
                                                sender: ChannelIdentity {
                                                    platform_id: slack_msg["user"]
                                                        .as_str()
                                                        .unwrap_or("")
                                                        .to_string(),
                                                    display_name: None,
                                                },
                                                content: MessageContent::Text(text.to_string()),
                                                thread_id: slack_msg["thread_ts"]
                                                    .as_str()
                                                    .map(String::from),
                                                timestamp: chrono::Utc::now(),
                                                raw: slack_msg.clone(),
                                            };
                                            let _ = tx.send(inbound).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => warn!("Slack poll error: {}", e),
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        let resp = self
            .client
            .get("https://slack.com/api/auth.test")
            .bearer_auth(self.bot_token.as_str())
            .send()
            .await;
        match resp {
            Ok(r) => {
                if let Ok(body) = r.json::<serde_json::Value>().await {
                    if body["ok"].as_bool() == Some(true) {
                        return ChannelHealth::Connected;
                    }
                    return ChannelHealth::Degraded(
                        body["error"].as_str().unwrap_or("unknown").to_string(),
                    );
                }
                ChannelHealth::Degraded("parse error".to_string())
            }
            Err(e) => ChannelHealth::Disconnected(e.to_string()),
        }
    }
}
