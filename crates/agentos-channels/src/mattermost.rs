/// Mattermost channel adapter using REST API + WebSocket.
use crate::types::{
    ChannelCapabilities, ChannelIdentity, DeliveryReceipt, InboundMessage, MessageContent,
    OutboundMessage,
};
use crate::{ChannelAdapter, ChannelHealth};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;
use zeroize::Zeroizing;

pub struct MattermostAdapter {
    client: Client,
    base_url: String,
    token: Zeroizing<String>,
    default_channel_id: String,
    name: String,
}

impl MattermostAdapter {
    pub fn new(
        base_url: String,
        token: String,
        default_channel_id: String,
    ) -> Result<Self, agentos_types::AgentOSError> {
        crate::webhook::validate_server_base_url(&base_url, "mattermost")?;
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("HTTP client build failed"),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: Zeroizing::new(token),
            default_channel_id,
            name: "mattermost".to_string(),
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/api/v4{}", self.base_url, path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token.as_str())
    }
}

#[async_trait]
impl ChannelAdapter for MattermostAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: true,
            reactions: true,
            media: true,
            rich_formatting: true,
            max_message_length: 16_383,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let text = msg.content.as_text();
        let channel_id = if msg.channel_instance_id.is_empty() {
            self.default_channel_id.clone()
        } else {
            msg.channel_instance_id.clone()
        };

        let body = json!({
            "channel_id": channel_id,
            "message": text,
            "root_id": msg.thread_id.unwrap_or_default(),
        });

        let response = self
            .client
            .post(self.api_url("/posts"))
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "mattermost".into(),
                reason: format!("HTTP request failed: {e}"),
            })?;

        if !response.status().is_success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "mattermost".into(),
                reason: format!("API returned HTTP {}", response.status()),
            });
        }

        let json_resp: Value = response.json().await.unwrap_or_default();
        Ok(DeliveryReceipt {
            message_id: json_resp["id"]
                .as_str()
                .unwrap_or(&Uuid::new_v4().to_string())
                .to_string(),
            delivered_at: Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        let ws_url = self
            .base_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        let ws_url = format!("{}/api/v4/websocket", ws_url);
        // Clone as Zeroizing<String> to keep the secret zeroizable.
        // It will be embedded in the auth JSON frame only for the duration of the send.
        let token: Zeroizing<String> = self.token.clone();

        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream,
        };

        let (mut ws_stream, _): (WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, _) =
            connect_async(&ws_url)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "mattermost".into(),
                    reason: format!("WebSocket connect failed: {e}"),
                })?;

        let auth_msg = json!({
            "seq": 1,
            "action": "authentication_challenge",
            "data": { "token": token.as_str() }
        });
        ws_stream
            .send(Message::Text(auth_msg.to_string()))
            .await
            .ok();

        info!("Mattermost WebSocket listener started");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = ws_stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            let val: Value = serde_json::from_str(&text).unwrap_or_default();
                            if val["event"].as_str() == Some("posted") {
                                if let Some(data) = val["data"].as_object() {
                                    let post: Value = serde_json::from_str(
                                        data.get("post").and_then(|v| v.as_str()).unwrap_or("{}"),
                                    ).unwrap_or_default();
                                    let inbound = InboundMessage {
                                        id: post["id"].as_str().unwrap_or("").to_string(),
                                        channel_type: "mattermost".to_string(),
                                        channel_instance_id: post["channel_id"].as_str().unwrap_or("").to_string(),
                                        sender: ChannelIdentity {
                                            platform_id: post["user_id"].as_str().unwrap_or("").to_string(),
                                            display_name: None,
                                        },
                                        content: MessageContent::Text(
                                            post["message"].as_str().unwrap_or("").to_string(),
                                        ),
                                        thread_id: post["root_id"].as_str()
                                            .filter(|s| !s.is_empty())
                                            .map(String::from),
                                        timestamp: Utc::now(),
                                        raw: post,
                                    };
                                    let _ = tx.send(inbound).await;
                                }
                            }
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "Mattermost WebSocket error");
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        match self
            .client
            .get(self.api_url("/system/ping"))
            .header("Authorization", self.auth_header())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => ChannelHealth::Connected,
            Ok(r) => ChannelHealth::Degraded(format!("HTTP {}", r.status())),
            Err(e) => {
                warn!(error = %e, "Mattermost health check failed");
                ChannelHealth::Disconnected(e.to_string())
            }
        }
    }
}
