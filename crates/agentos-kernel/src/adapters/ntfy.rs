use crate::notification_router::{DeliveryAdapter, DeliveryError, InboundMessage};
use agentos_types::{
    ChannelInstanceID, DeliveryChannel, NotificationPriority, UserMessage, UserMessageKind,
};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;

/// ntfy.sh (or self-hosted ntfy) delivery and inbound adapter.
///
/// Outbound: HTTP PUT to `{server}/{topic}` with ntfy-specific headers.
/// Inbound: subscribes to `{server}/{reply_topic}/sse` and yields inbound
/// messages to the `InboundRouter`.  ntfy action buttons are supported for
/// `Question` messages — each option generates an HTTP action pointing to the
/// AgentOS webhook (if configured), falling back to a view action.
pub struct NtfyDeliveryAdapter {
    server_url: String,
    topic: String,
    reply_topic: String,
    access_token: Option<String>,
    channel_instance_id: ChannelInstanceID,
    client: reqwest::Client,
}

impl NtfyDeliveryAdapter {
    pub fn new(
        server_url: String,
        topic: String,
        reply_topic: String,
        access_token: Option<String>,
        channel_instance_id: ChannelInstanceID,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self {
            server_url,
            topic,
            reply_topic,
            access_token,
            channel_instance_id,
            client,
        }
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.access_token {
            req.bearer_auth(token)
        } else {
            req
        }
    }
}

#[async_trait]
impl DeliveryAdapter for NtfyDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::custom(DeliveryChannel::NTFY)
    }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        let priority = priority_to_ntfy(&msg.priority);
        let url = format!("{}/{}", self.server_url, self.topic);
        let body: String = msg.body.chars().take(4096).collect();

        let mut req = self
            .client
            .put(&url)
            .header("Title", &msg.subject)
            .header("Priority", priority)
            .header("Tags", kind_to_ntfy_tag(&msg.kind))
            .body(body);

        // Add action buttons for Question messages.
        if let UserMessageKind::Question {
            options: Some(opts),
            ..
        } = &msg.kind
        {
            if !opts.is_empty() {
                let actions: Vec<String> = opts
                    .iter()
                    .map(|opt| {
                        // Escape ntfy action-string separators in the label so
                        // commas/semicolons in option text don't corrupt parsing.
                        let label = opt.replace(',', "\\,").replace(';', "\\;");
                        // Percent-encode the path segment so special characters
                        // (/, ?, #, spaces) don't produce invalid URLs.
                        let encoded_path = percent_encoding::utf8_percent_encode(
                            opt,
                            percent_encoding::NON_ALPHANUMERIC,
                        )
                        .to_string();
                        format!(
                            "view, {label}, {}/{}/{}",
                            self.server_url, self.reply_topic, encoded_path
                        )
                    })
                    .collect();
                req = req.header("Actions", actions.join("; "));
            }
        }

        req = self.apply_auth(req);

        let resp = req
            .send()
            .await
            // Suppress the error value — it may contain the access token.
            .map_err(|_| DeliveryError("ntfy PUT request failed".into()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            return Err(DeliveryError(format!("ntfy PUT HTTP {status}")));
        }
        Ok(())
    }

    async fn is_available(&self) -> bool {
        !self.server_url.is_empty() && !self.topic.is_empty()
    }

    fn adapter_instance_id(&self) -> Option<String> {
        Some(self.channel_instance_id.to_string())
    }

    fn supports_inbound(&self) -> bool {
        !self.reply_topic.is_empty()
    }

    async fn start_listening(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Result<tokio::task::JoinHandle<()>, DeliveryError> {
        let server_url = self.server_url.clone();
        let reply_topic = self.reply_topic.clone();
        let access_token = self.access_token.clone();
        let channel_instance_id = self.channel_instance_id;
        let client = self.client.clone();

        let handle = tokio::spawn(async move {
            ntfy_sse_loop(
                server_url,
                reply_topic,
                access_token,
                channel_instance_id,
                client,
                tx,
            )
            .await;
        });
        Ok(handle)
    }
}

/// Subscribe to the ntfy reply topic via SSE and forward events to `tx`.
async fn ntfy_sse_loop(
    server_url: String,
    reply_topic: String,
    access_token: Option<String>,
    channel_instance_id: ChannelInstanceID,
    _client: reqwest::Client,
    tx: mpsc::Sender<InboundMessage>,
) {
    // SSE connections are long-lived; use a client without a read timeout so the
    // stream is not killed when the topic is idle for >120s.
    let sse_client = reqwest::Client::builder().build().unwrap_or_default();
    loop {
        let url = format!("{server_url}/{reply_topic}/sse");
        let mut req = sse_client.get(&url);
        if let Some(token) = &access_token {
            req = req.bearer_auth(token);
        }

        match req.send().await {
            Ok(resp) => {
                let mut stream = resp.bytes_stream();
                let mut buf = String::new();
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(bytes) => {
                            if let Ok(text) = std::str::from_utf8(&bytes) {
                                buf.push_str(text);
                                // Guard against unbounded buffer growth from a malformed server.
                                if buf.len() > 65_536 {
                                    tracing::warn!("ntfy SSE buffer exceeded 64KB without newline; reconnecting");
                                    break;
                                }
                                // Process complete SSE lines (terminated by '\n')
                                while let Some(pos) = buf.find('\n') {
                                    let line = buf[..pos].trim().to_string();
                                    buf = buf[pos + 1..].to_string();
                                    if let Some(data) = line.strip_prefix("data: ") {
                                        if let Ok(event) = serde_json::from_str::<NtfyEvent>(data) {
                                            if event.event == "message" {
                                                let inbound = InboundMessage {
                                                    channel: DeliveryChannel::custom(
                                                        DeliveryChannel::NTFY,
                                                    ),
                                                    channel_instance_id,
                                                    external_sender_id: event.topic.clone(),
                                                    text: event.message.clone(),
                                                    reply_to_notification_id: None,
                                                    received_at: Utc::now(),
                                                    raw: serde_json::json!({
                                                        "topic": event.topic,
                                                        "message": event.message,
                                                    }),
                                                };
                                                if tx.send(inbound).await.is_err() {
                                                    // Receiver dropped — kernel shutdown.
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("ntfy SSE chunk error: {e}");
                            break; // reconnect
                        }
                    }
                }
            }
            Err(_) => {
                // Suppress the error value — the URL includes the access token as a bearer header
                // and some reqwest builds include header details in the error message.
                tracing::warn!("ntfy SSE connect failed (details redacted); retrying in 10s");
            }
        }
        // Brief pause before reconnect to avoid hammering a down server.
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

fn priority_to_ntfy(p: &NotificationPriority) -> &'static str {
    match p {
        NotificationPriority::Critical => "5",
        NotificationPriority::Urgent => "4",
        NotificationPriority::Warning => "3",
        NotificationPriority::Info => "2",
    }
}

fn kind_to_ntfy_tag(kind: &UserMessageKind) -> &'static str {
    match kind {
        UserMessageKind::TaskComplete { .. } => "white_check_mark",
        UserMessageKind::Question { .. } => "question",
        UserMessageKind::StatusUpdate { .. } => "information_source",
        UserMessageKind::Notification => "bell",
    }
}

#[derive(Debug, Deserialize)]
struct NtfyEvent {
    #[serde(default)]
    event: String,
    #[serde(default)]
    topic: String,
    #[serde(default)]
    message: String,
}
