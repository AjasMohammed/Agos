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

/// Build inbound message content from a Discord `MESSAGE_CREATE` `d` object,
/// combining the text body and any `attachments` (public CDN URLs). Image
/// attachments (by `content_type`) become `MessageContent::Image` so the kernel
/// feeds them to vision; others become `File`. Returns `None` when there is
/// neither text nor a usable attachment (nothing to forward).
fn discord_message_content(d: &serde_json::Value) -> Option<MessageContent> {
    let text = d["content"].as_str().unwrap_or("");
    let mut media: Vec<MessageContent> = Vec::new();
    if let Some(atts) = d["attachments"].as_array() {
        for att in atts {
            let url = match att["url"].as_str() {
                Some(u) if !u.is_empty() => u.to_string(),
                _ => continue,
            };
            let filename = att["filename"].as_str().unwrap_or("file").to_string();
            let mime = att["content_type"].as_str().unwrap_or("");
            if mime.starts_with("image/") {
                media.push(MessageContent::Image {
                    url,
                    alt: (!filename.is_empty()).then(|| filename.clone()),
                });
            } else {
                media.push(MessageContent::File {
                    url,
                    filename,
                    mime: if mime.is_empty() {
                        "application/octet-stream".to_string()
                    } else {
                        mime.to_string()
                    },
                });
            }
        }
    }
    match (text.trim().is_empty(), media.len()) {
        (true, 0) => None,
        (false, 0) => Some(MessageContent::Text(text.to_string())),
        (true, 1) => media.into_iter().next(),
        _ => {
            let mut parts = Vec::new();
            if !text.trim().is_empty() {
                parts.push(MessageContent::Text(text.to_string()));
            }
            parts.extend(media);
            Some(MessageContent::Mixed(parts))
        }
    }
}

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
        let text: String = msg
            .content
            .render_for_delivery()
            .chars()
            .take(2000)
            .collect();
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
                                                // Skip bot/self-authored messages so the bot's own
                                                // posts don't echo back as inbound (loop guard).
                                                if d["channel_id"].as_str() == Some(&channel_id)
                                                    && d["author"]["bot"].as_bool() != Some(true)
                                                {
                                                    // Combine text + attachments (images → vision, files → note).
                                                    if let Some(content) = discord_message_content(d) {
                                                        let inbound = InboundMessage {
                                                            id: d["id"].as_str().unwrap_or("").to_string(),
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
                                                            content,
                                                            thread_id: None,
                                                            timestamp: chrono::Utc::now(),
                                                            raw: payload.clone(),
                                                        };
                                                        let _ = tx.send(inbound).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_only_message() {
        let d = json!({ "content": "hello", "attachments": [] });
        assert!(matches!(
            discord_message_content(&d),
            Some(MessageContent::Text(t)) if t == "hello"
        ));
    }

    #[test]
    fn empty_message_is_none() {
        let d = json!({ "content": "", "attachments": [] });
        assert!(discord_message_content(&d).is_none());
    }

    #[test]
    fn image_only_becomes_image() {
        let d = json!({
            "content": "",
            "attachments": [
                { "url": "https://cdn.discordapp.com/a/cat.png", "filename": "cat.png", "content_type": "image/png" }
            ]
        });
        match discord_message_content(&d) {
            Some(MessageContent::Image { url, alt }) => {
                assert_eq!(url, "https://cdn.discordapp.com/a/cat.png");
                assert_eq!(alt.as_deref(), Some("cat.png"));
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn text_plus_image_becomes_mixed() {
        let d = json!({
            "content": "look",
            "attachments": [
                { "url": "https://cdn.discordapp.com/a/x.jpg", "filename": "x.jpg", "content_type": "image/jpeg" }
            ]
        });
        match discord_message_content(&d) {
            Some(MessageContent::Mixed(parts)) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[0], MessageContent::Text(t) if t == "look"));
                assert!(matches!(&parts[1], MessageContent::Image { .. }));
            }
            other => panic!("expected Mixed, got {other:?}"),
        }
    }

    #[test]
    fn non_image_becomes_file_with_default_mime() {
        let d = json!({
            "content": "",
            "attachments": [
                { "url": "https://cdn.discordapp.com/a/report", "filename": "report.pdf" }
            ]
        });
        match discord_message_content(&d) {
            Some(MessageContent::File { mime, filename, .. }) => {
                assert_eq!(filename, "report.pdf");
                assert_eq!(mime, "application/octet-stream");
            }
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn attachment_without_url_is_skipped() {
        let d = json!({ "content": "", "attachments": [ { "filename": "x" } ] });
        assert!(discord_message_content(&d).is_none());
    }

    #[test]
    fn text_plus_multiple_attachments_keeps_order() {
        let d = json!({
            "content": "hi",
            "attachments": [
                { "url": "https://cdn/x.png", "filename": "x.png", "content_type": "image/png" },
                { "url": "https://cdn/y.pdf", "filename": "y.pdf", "content_type": "application/pdf" }
            ]
        });
        match discord_message_content(&d) {
            Some(MessageContent::Mixed(parts)) => {
                assert_eq!(parts.len(), 3);
                assert!(matches!(&parts[0], MessageContent::Text(t) if t == "hi"));
                assert!(matches!(&parts[1], MessageContent::Image { .. }));
                assert!(matches!(&parts[2], MessageContent::File { .. }));
            }
            other => panic!("expected Mixed, got {other:?}"),
        }
    }

    #[test]
    fn url_less_attachment_skipped_keeps_valid_one() {
        let d = json!({
            "content": "",
            "attachments": [
                { "filename": "broken" },
                { "url": "https://cdn/ok.png", "filename": "ok.png", "content_type": "image/png" }
            ]
        });
        assert!(matches!(
            discord_message_content(&d),
            Some(MessageContent::Image { .. })
        ));
    }

    #[test]
    fn whitespace_only_text_is_none() {
        let d = json!({ "content": "   ", "attachments": [] });
        assert!(discord_message_content(&d).is_none());
    }
}
