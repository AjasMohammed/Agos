/// Mattermost channel adapter using REST API + WebSocket.
use crate::types::{
    ChannelCapabilities, ChannelIdentity, DeliveryReceipt, InboundMessage, MessageContent,
    OutboundMessage,
};
use crate::{ChannelAdapter, ChannelHealth};
use agentos_http::{client, HttpProfile};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;
use zeroize::Zeroizing;

/// Build inbound content from a Mattermost `posted` event's parsed `post` plus
/// any attached files. File bytes live at `{base_url}/api/v4/files/{id}` and
/// require the bot token to download — which the kernel supplies, gated to the
/// channel's own server host. Prefers `metadata.files` (has name + mime);
/// falls back to bare `file_ids` (kernel sniffs the type post-download).
fn mattermost_message_content(post: &serde_json::Value, base_url: &str) -> Option<MessageContent> {
    let text = post["message"].as_str().unwrap_or("");
    let base = base_url.trim_end_matches('/');
    let mut media: Vec<MessageContent> = Vec::new();
    if let Some(files) = post["metadata"]["files"]
        .as_array()
        .filter(|a| !a.is_empty())
    {
        for f in files {
            let id = match f["id"].as_str() {
                Some(i) if !i.is_empty() => i,
                _ => continue,
            };
            let url = format!("{base}/api/v4/files/{id}");
            let filename = f["name"].as_str().unwrap_or("file").to_string();
            let mime = f["mime_type"].as_str().unwrap_or("");
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
    } else if let Some(ids) = post["file_ids"].as_array() {
        for fid in ids {
            if let Some(id) = fid.as_str().filter(|s| !s.is_empty()) {
                // No name/mime in the event — emit as File; the kernel sniffs the
                // MIME after download and routes images to vision regardless.
                media.push(MessageContent::File {
                    url: format!("{base}/api/v4/files/{id}"),
                    filename: "file".to_string(),
                    mime: String::new(),
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
            client: client(HttpProfile::Outbound),
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
        let text = msg.content.render_for_delivery();
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

        // Resolve our own user id once so we can skip the bot's own posts —
        // the `posted` event fires for our outbound messages too (echo loop).
        let self_user_id: Option<String> = match self
            .client
            .get(self.api_url("/users/me"))
            .header("Authorization", self.auth_header())
            .send()
            .await
        {
            Ok(r) => r
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["id"].as_str().map(String::from)),
            Err(_) => None,
        };
        if self_user_id.is_none() {
            warn!("Mattermost: could not resolve own user id; self-echo filtering disabled");
        }

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
                                    let from_self = matches!(
                                        self_user_id.as_deref(),
                                        Some(uid) if post["user_id"].as_str() == Some(uid)
                                    );
                                    if from_self {
                                        continue;
                                    }
                                    if let Some(content) =
                                        mattermost_message_content(&post, &self.base_url)
                                    {
                                        let inbound = InboundMessage {
                                            id: post["id"].as_str().unwrap_or("").to_string(),
                                            channel_type: "mattermost".to_string(),
                                            channel_instance_id: post["channel_id"].as_str().unwrap_or("").to_string(),
                                            sender: ChannelIdentity {
                                                platform_id: post["user_id"].as_str().unwrap_or("").to_string(),
                                                display_name: None,
                                            },
                                            content,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_only() {
        let p = json!({ "message": "hello" });
        assert!(matches!(
            mattermost_message_content(&p, "https://mm.example.com"),
            Some(MessageContent::Text(t)) if t == "hello"
        ));
    }

    #[test]
    fn empty_is_none() {
        let p = json!({ "message": "" });
        assert!(mattermost_message_content(&p, "https://mm.example.com").is_none());
    }

    #[test]
    fn metadata_image_builds_url() {
        let p = json!({
            "message": "",
            "metadata": { "files": [
                { "id": "f1", "name": "cat.png", "mime_type": "image/png" }
            ]}
        });
        match mattermost_message_content(&p, "https://mm.example.com/") {
            Some(MessageContent::Image { url, .. }) => {
                assert_eq!(url, "https://mm.example.com/api/v4/files/f1");
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn file_ids_fallback_builds_file_url() {
        let p = json!({ "message": "doc", "file_ids": ["abc"] });
        match mattermost_message_content(&p, "https://mm.example.com") {
            Some(MessageContent::Mixed(parts)) => {
                assert!(matches!(&parts[0], MessageContent::Text(t) if t == "doc"));
                assert!(matches!(
                    &parts[1],
                    MessageContent::File { url, .. } if url == "https://mm.example.com/api/v4/files/abc"
                ));
            }
            other => panic!("expected Mixed, got {other:?}"),
        }
    }
}
