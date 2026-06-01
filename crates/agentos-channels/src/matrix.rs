/// Matrix channel adapter using the Client-Server HTTP API (no heavy matrix-sdk dependency).
///
/// Inbound: long-polls /sync with 30s timeout.
/// Outbound: PUT /rooms/{room_id}/send/m.room.message/{txn_id}
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
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;
use zeroize::Zeroizing;

/// Resolve a Matrix `mxc://server/mediaid` URI to a downloadable homeserver URL.
///
/// Uses the **authenticated** client endpoint (`/_matrix/client/v1/media/download`)
/// introduced in Matrix 1.11 (MSC3916) — homeservers now enforce authenticated
/// media, and the kernel sends the access token (gated to the homeserver host).
/// The legacy `/_matrix/media/v3/download` path is unauthenticated and rejected
/// by modern servers.
fn mxc_to_download_url(homeserver: &str, mxc: &str) -> Option<String> {
    let rest = mxc.strip_prefix("mxc://")?;
    let (server, media_id) = rest.split_once('/')?;
    if server.is_empty() || media_id.is_empty() {
        return None;
    }
    Some(format!(
        "{}/_matrix/client/v1/media/download/{}/{}",
        homeserver.trim_end_matches('/'),
        server,
        media_id
    ))
}

/// Build inbound content from a Matrix `m.room.message` event `content`. Media
/// msgtypes (`m.image`/`m.file`/`m.video`/`m.audio`) carry an `mxc://` URL that
/// resolves against the homeserver and requires the access token to download
/// (supplied kernel-side, gated to the homeserver host). `m.text` → text.
fn matrix_message_content(content: &serde_json::Value, homeserver: &str) -> Option<MessageContent> {
    let msgtype = content["msgtype"].as_str().unwrap_or("m.text");
    let body = content["body"].as_str().unwrap_or("");
    match msgtype {
        "m.image" | "m.file" | "m.video" | "m.audio" => {
            // Cleartext media carries `content.url` (mxc://). Encrypted rooms put
            // it at `content.file.url` with keys we can't use here — in that case
            // fall back to the body caption rather than dropping the message.
            let url = match content["url"]
                .as_str()
                .and_then(|mxc| mxc_to_download_url(homeserver, mxc))
            {
                Some(u) => u,
                None => {
                    return if body.trim().is_empty() {
                        None
                    } else {
                        Some(MessageContent::Text(body.to_string()))
                    };
                }
            };
            let mime = content["info"]["mimetype"].as_str().unwrap_or("");
            let filename = if body.is_empty() { "file" } else { body };
            if msgtype == "m.image" || mime.starts_with("image/") {
                Some(MessageContent::Image {
                    url,
                    alt: (!filename.is_empty()).then(|| filename.to_string()),
                })
            } else {
                Some(MessageContent::File {
                    url,
                    filename: filename.to_string(),
                    mime: if mime.is_empty() {
                        "application/octet-stream".to_string()
                    } else {
                        mime.to_string()
                    },
                })
            }
        }
        _ => {
            if body.trim().is_empty() {
                None
            } else {
                Some(MessageContent::Text(body.to_string()))
            }
        }
    }
}

pub struct MatrixAdapter {
    client: Client,
    homeserver: String,
    access_token: Zeroizing<String>,
    /// Rooms to listen in. Empty = all joined rooms.
    rooms: Vec<String>,
    name: String,
    /// /sync pagination token, updated after each poll.
    since: Arc<Mutex<Option<String>>>,
}

impl MatrixAdapter {
    pub fn new(
        homeserver: String,
        access_token: String,
        rooms: Vec<String>,
    ) -> Result<Self, agentos_types::AgentOSError> {
        crate::webhook::validate_server_base_url(&homeserver, "matrix")?;
        Ok(Self {
            client: client(HttpProfile::Outbound),
            homeserver: homeserver.trim_end_matches('/').to_string(),
            access_token: Zeroizing::new(access_token),
            rooms,
            name: "matrix".to_string(),
            since: Arc::new(Mutex::new(None)),
        })
    }

    fn cs_url(&self, path: &str) -> String {
        format!("{}/_matrix/client/v3{}", self.homeserver, path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.access_token.as_str())
    }
}

#[async_trait]
impl ChannelAdapter for MatrixAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: true,
            reactions: true,
            media: true,
            rich_formatting: true,
            max_message_length: 65_536,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let room_id = urlencoding::encode(&msg.channel_instance_id).to_string();
        let text = msg.content.render_for_delivery();
        let txn_id = Uuid::new_v4();

        let body = json!({ "msgtype": "m.text", "body": text });
        let url = self.cs_url(&format!(
            "/rooms/{}/send/m.room.message/{}",
            room_id, txn_id
        ));

        let response = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "matrix".into(),
                reason: format!("HTTP request failed: {e}"),
            })?;

        if !response.status().is_success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "matrix".into(),
                reason: format!("Matrix API returned HTTP {}", response.status()),
            });
        }

        let resp: Value = response.json().await.unwrap_or_default();
        Ok(DeliveryReceipt {
            message_id: resp["event_id"]
                .as_str()
                .unwrap_or(&txn_id.to_string())
                .to_string(),
            delivered_at: Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        let sync_url = self.cs_url("/sync");
        let auth = self.auth_header();
        let client = self.client.clone();
        let since_lock = Arc::clone(&self.since);
        let rooms = self.rooms.clone();

        // Resolve our own MXID once so we can skip the bot's own messages —
        // /sync always echoes events we sent, which would otherwise loop.
        let self_user_id: Option<String> = match client
            .get(self.cs_url("/account/whoami"))
            .header("Authorization", &auth)
            .send()
            .await
        {
            Ok(r) => r
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["user_id"].as_str().map(String::from)),
            Err(_) => None,
        };
        if self_user_id.is_none() {
            warn!("Matrix: could not resolve own MXID; self-echo filtering disabled");
        }

        info!("Matrix long-poll listener started");

        loop {
            if cancel.is_cancelled() {
                break;
            }

            let since_val = since_lock.lock().await.clone();
            let mut req = client
                .get(&sync_url)
                .header("Authorization", &auth)
                .query(&[("timeout", "30000")]);
            if let Some(ref s) = since_val {
                req = req.query(&[("since", s.as_str())]);
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Matrix sync failed");
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            let sync: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "Failed to parse Matrix sync");
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            if let Some(next_batch) = sync["next_batch"].as_str() {
                *since_lock.lock().await = Some(next_batch.to_string());
            }

            if let Some(joined) = sync["rooms"]["join"].as_object() {
                for (room_id, room_data) in joined {
                    if !rooms.is_empty() && !rooms.contains(room_id) {
                        continue;
                    }
                    let events = room_data["timeline"]["events"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    for event in events {
                        if event["type"].as_str() != Some("m.room.message") {
                            continue;
                        }
                        // Skip our own echoed messages to avoid a reply loop.
                        if matches!(self_user_id.as_deref(), Some(uid) if event["sender"].as_str() == Some(uid))
                        {
                            continue;
                        }
                        let content =
                            match matrix_message_content(&event["content"], &self.homeserver) {
                                Some(c) => c,
                                None => continue,
                            };
                        let _ = tx
                            .send(InboundMessage {
                                id: event["event_id"].as_str().unwrap_or("").to_string(),
                                channel_type: "matrix".to_string(),
                                channel_instance_id: room_id.clone(),
                                sender: ChannelIdentity {
                                    platform_id: event["sender"].as_str().unwrap_or("").to_string(),
                                    display_name: None,
                                },
                                content,
                                thread_id: event["content"]["m.relates_to"]["event_id"]
                                    .as_str()
                                    .map(String::from),
                                timestamp: Utc::now(),
                                raw: event,
                            })
                            .await;
                    }
                }
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        match self
            .client
            .get(self.cs_url("/account/whoami"))
            .header("Authorization", self.auth_header())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => ChannelHealth::Connected,
            Ok(r) => ChannelHealth::Degraded(format!("HTTP {}", r.status())),
            Err(e) => {
                warn!(error = %e, "Matrix health check failed");
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
    fn mxc_resolution() {
        assert_eq!(
            mxc_to_download_url("https://hs.example.com", "mxc://m.org/abc123"),
            Some(
                "https://hs.example.com/_matrix/client/v1/media/download/m.org/abc123".to_string()
            )
        );
        assert!(mxc_to_download_url("https://hs", "not-an-mxc").is_none());
        assert!(mxc_to_download_url("https://hs", "mxc://only-server").is_none());
    }

    #[test]
    fn text_message() {
        let c = json!({ "msgtype": "m.text", "body": "hi" });
        assert!(matches!(
            matrix_message_content(&c, "https://hs"),
            Some(MessageContent::Text(t)) if t == "hi"
        ));
    }

    #[test]
    fn image_message_resolves_mxc() {
        let c = json!({
            "msgtype": "m.image",
            "body": "cat.png",
            "url": "mxc://hs.org/xyz",
            "info": { "mimetype": "image/png" }
        });
        match matrix_message_content(&c, "https://hs.org") {
            Some(MessageContent::Image { url, .. }) => {
                assert_eq!(
                    url,
                    "https://hs.org/_matrix/client/v1/media/download/hs.org/xyz"
                );
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn file_message_becomes_file() {
        let c = json!({
            "msgtype": "m.file",
            "body": "report.pdf",
            "url": "mxc://hs.org/doc1",
            "info": { "mimetype": "application/pdf" }
        });
        assert!(matches!(
            matrix_message_content(&c, "https://hs.org"),
            Some(MessageContent::File { mime, .. }) if mime == "application/pdf"
        ));
    }

    #[test]
    fn media_without_url_falls_back_to_caption_or_none() {
        // Encrypted/malformed media (no resolvable mxc) but with a body → keep
        // the caption text rather than dropping the whole message.
        let c = json!({ "msgtype": "m.image", "body": "x" });
        assert!(matches!(
            matrix_message_content(&c, "https://hs"),
            Some(MessageContent::Text(t)) if t == "x"
        ));
        // No url and no body → nothing to forward.
        let empty = json!({ "msgtype": "m.image", "body": "" });
        assert!(matrix_message_content(&empty, "https://hs").is_none());
    }
}
