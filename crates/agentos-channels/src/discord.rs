use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_http::{client, HttpProfile};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::error;
use zeroize::Zeroizing;

pub struct DiscordAdapter {
    bot_token: Zeroizing<String>,
    pub channel_id: String,
    pub instance_id: String,
    client: reqwest::Client,
    /// Set to `false` when the Gateway WS listener exits, so `health_check`
    /// reflects listener death rather than only REST reachability.
    listener_alive: Arc<AtomicBool>,
}

impl DiscordAdapter {
    pub fn new(bot_token: String, channel_id: String, instance_id: String) -> Self {
        Self {
            bot_token: Zeroizing::new(bot_token),
            channel_id,
            instance_id,
            client: client(HttpProfile::Outbound),
            listener_alive: Arc::new(AtomicBool::new(false)),
        }
    }

    fn rest_url(&self, path: &str) -> String {
        format!("https://discord.com/api/v10{}", path)
    }

    fn auth_header(&self) -> String {
        format!("Bot {}", self.bot_token.as_str())
    }
}

#[async_trait]
impl ChannelAdapter for DiscordAdapter {
    fn name(&self) -> &str {
        "discord"
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: true,
            reactions: true,
            media: true,
            rich_formatting: true,
            max_message_length: 2000,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let text = msg.content.as_text();
        let url = self.rest_url(&format!("/channels/{}/messages", self.channel_id));
        let client = &self.client;
        let auth = self.auth_header();
        let policy = crate::retry::RetryPolicy::default();

        crate::retry::with_retry(&policy, "discord", || async {
            let resp = client
                .post(&url)
                .header("Authorization", &auth)
                .json(&serde_json::json!({"content": &text}))
                .send()
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "discord".to_string(),
                    reason: e.to_string(),
                })?;
            if !resp.status().is_success() {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "discord".to_string(),
                    reason: format!("Discord API error: {}", resp.status()),
                });
            }
            Ok(DeliveryReceipt {
                message_id: uuid::Uuid::new_v4().to_string(),
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
        let gateway_url = "wss://gateway.discord.gg/?v=10&encoding=json";
        let instance_id = self.instance_id.clone();
        let channel_id = self.channel_id.clone();
        let listener_alive = self.listener_alive.clone();

        let mut reconnect_delay = std::time::Duration::from_secs(1);
        let max_reconnect_delay = std::time::Duration::from_secs(60);

        // Outer reconnect loop — re-establishes the WebSocket on disconnect.
        loop {
            if cancel.is_cancelled() {
                break;
            }

            let connect_result = tokio_tungstenite::connect_async(gateway_url).await;
            let ws_stream = match connect_result {
                Ok((ws, _)) => {
                    reconnect_delay = std::time::Duration::from_secs(1); // reset on success
                    ws
                }
                Err(e) => {
                    error!(
                        "Discord WS connect failed: {e}, retrying in {:?}",
                        reconnect_delay
                    );
                    listener_alive.store(false, Ordering::Release);
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(reconnect_delay) => {},
                    }
                    reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
                    continue;
                }
            };

            let (mut write, mut read) = ws_stream.split();
            let mut heartbeat_interval: Option<tokio::time::Interval> = None;
            let mut sequence: Option<u64> = None;
            // Fresh token for each connection attempt.
            let mut bot_token: Option<zeroize::Zeroizing<String>> = Some(self.bot_token.clone());

            // Inner message loop — processes messages until disconnect.
            loop {
                if cancel.is_cancelled() {
                    break;
                }

                let tick = async {
                    if let Some(ref mut interval) = heartbeat_interval {
                        interval.tick().await;
                        true
                    } else {
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                        false
                    }
                };

                tokio::select! {
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text) {
                                    let op = payload["op"].as_u64().unwrap_or(255);

                                    match op {
                                        10 => { // HELLO
                                            let interval_ms = payload["d"]["heartbeat_interval"]
                                                .as_u64()
                                                .unwrap_or(41250);
                                            // Per Discord docs: jitter the first heartbeat to avoid
                                            // thundering herd on reconnect.
                                            let jitter_ms = rand::thread_rng().gen_range(0..interval_ms);
                                            heartbeat_interval = Some(tokio::time::interval_at(
                                                tokio::time::Instant::now()
                                                    + std::time::Duration::from_millis(jitter_ms),
                                                std::time::Duration::from_millis(interval_ms),
                                            ));
                                            if let Some(token) = bot_token.take() {
                                                // GUILD_MESSAGES(512) | MESSAGE_CONTENT(32768) = 33280
                                                let identify = serde_json::json!({
                                                    "op": 2,
                                                    "d": {
                                                        "token": token.as_str(),
                                                        "intents": 33280,
                                                        "properties": {
                                                            "os": "linux",
                                                            "browser": "agentos",
                                                            "device": "agentos"
                                                        }
                                                    }
                                                });
                                                let _ = write.send(Message::Text(identify.to_string())).await;
                                            }
                                        }
                                        0 => { // DISPATCH — only DISPATCH carries meaningful sequence numbers
                                            // Update sequence cursor only on DISPATCH events.
                                            if let Some(seq) = payload["s"].as_u64() {
                                                sequence = Some(seq);
                                            }
                                            let event_type = payload["t"].as_str();
                                            if event_type == Some("READY") {
                                                // Gateway is ready and authenticated.
                                                listener_alive.store(true, Ordering::Release);
                                            }
                                            if event_type == Some("MESSAGE_CREATE") {
                                                let d = &payload["d"];
                                                if d["channel_id"].as_str() == Some(&channel_id) {
                                                    if let Some(content) = d["content"].as_str() {
                                                        if !content.is_empty() {
                                                            let inbound = InboundMessage {
                                                                id: d["id"]
                                                                    .as_str()
                                                                    .unwrap_or("")
                                                                    .to_string(),
                                                                channel_type: "discord".to_string(),
                                                                channel_instance_id: instance_id.clone(),
                                                                sender: ChannelIdentity {
                                                                    platform_id: d["author"]["id"]
                                                                        .as_str()
                                                                        .unwrap_or("")
                                                                        .to_string(),
                                                                    display_name: d["author"]["username"]
                                                                        .as_str()
                                                                        .map(String::from),
                                                                },
                                                                content: MessageContent::Text(
                                                                    content.to_string(),
                                                                ),
                                                                thread_id: None,
                                                                timestamp: chrono::Utc::now(),
                                                                raw: payload.clone(),
                                                            };
                                                            let _ = tx.send(inbound).await;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                error!("Discord WS error: {e}, will reconnect");
                                break; // break inner loop to reconnect
                            }
                            None => break, // stream ended, reconnect
                            _ => {}
                        }
                    }
                    should_heartbeat = tick => {
                        if should_heartbeat {
                            let hb = serde_json::json!({"op": 1, "d": sequence});
                            let _ = write.send(Message::Text(hb.to_string())).await;
                        }
                    }
                }
            }

            listener_alive.store(false, Ordering::Release);

            // If cancelled, exit cleanly
            if cancel.is_cancelled() {
                break;
            }

            // Backoff before reconnect
            tracing::warn!(
                "Discord listener disconnected, reconnecting in {:?}",
                reconnect_delay
            );
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(reconnect_delay) => {},
            }
            reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
        }
        listener_alive.store(false, Ordering::Release);
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        // If the listener has started but is no longer alive, report degraded
        // regardless of REST reachability — inbound messages are not flowing.
        if !self.listener_alive.load(Ordering::Acquire) {
            // Listener not yet started (false on new()) or has exited.
            // Fall through to REST check; caller can infer listener state from logs.
        }

        let url = self.rest_url("/users/@me");
        match self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                if self.listener_alive.load(Ordering::Acquire) {
                    ChannelHealth::Connected
                } else {
                    ChannelHealth::Degraded(
                        "REST reachable but Gateway listener is not running".to_string(),
                    )
                }
            }
            Ok(r) => ChannelHealth::Degraded(format!("status {}", r.status())),
            Err(e) => ChannelHealth::Disconnected(e.to_string()),
        }
    }
}
