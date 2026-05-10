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
        let text = msg.content.as_text();
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
                        let text = event["content"]["body"].as_str().unwrap_or("").to_string();
                        if text.is_empty() {
                            continue;
                        }
                        let _ = tx
                            .send(InboundMessage {
                                id: event["event_id"].as_str().unwrap_or("").to_string(),
                                channel_type: "matrix".to_string(),
                                channel_instance_id: room_id.clone(),
                                sender: ChannelIdentity {
                                    platform_id: event["sender"].as_str().unwrap_or("").to_string(),
                                    display_name: None,
                                },
                                content: MessageContent::Text(text),
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
