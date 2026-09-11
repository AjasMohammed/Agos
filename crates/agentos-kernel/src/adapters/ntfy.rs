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

        // Action buttons: from `msg.actions` (escalations) when present,
        // otherwise from a `Question`'s options. Both encode the same way — a
        // `view` action whose URL path is the literal reply text, which the
        // reply-topic listener turns straight back into an InboundMessage.
        //
        // `(display label, reply text)` — for a `Question` the option text is
        // both; for an action the label is friendly ("✅ Approve") while the
        // reply is the command the InboundRouter parses ("/approve 42").
        let buttons: Vec<(&str, &str)> = if !msg.actions.is_empty() {
            msg.actions
                .iter()
                .map(|a| (a.label.as_str(), a.command.as_str()))
                .collect()
        } else if let UserMessageKind::Question {
            options: Some(opts),
            ..
        } = &msg.kind
        {
            opts.iter().map(|o| (o.as_str(), o.as_str())).collect()
        } else {
            Vec::new()
        };

        if !buttons.is_empty() {
            req = req.header(
                "Actions",
                ntfy_actions_header(
                    &self.server_url,
                    &self.reply_topic,
                    &buttons,
                    self.access_token.as_deref(),
                ),
            );
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
                                                    media_file_ids: Vec::new(),
                                                    pending_media: Vec::new(),
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

/// Build the ntfy `Actions` header for a set of `(display label, reply text)`
/// buttons.
///
/// Uses the `http` action, NOT `view`: `view` merely opens a URL in a browser,
/// so a tap would never publish anything and the reply-topic listener — which
/// only fires on a `message` event — would never see it. `http` POSTs the reply
/// text to the reply topic, which is exactly what a typed reply does, so the
/// tap reaches `InboundRouter` by the same path.
///
/// `clear=true` dismisses the notification once the request succeeds, so an
/// operator cannot double-approve by tapping twice.
fn ntfy_actions_header(
    server_url: &str,
    reply_topic: &str,
    buttons: &[(&str, &str)],
    access_token: Option<&str>,
) -> String {
    let publish_url = format!("{server_url}/{reply_topic}");
    buttons
        .iter()
        .map(|(label, reply)| {
            // `,` and `;` separate fields and actions in this header, so either
            // one inside a label silently corrupts every action after it.
            let label = sanitize_action_field(label);
            // Same for the body, which is a header value: a newline would
            // terminate it and `reqwest` refuses to build the header at all.
            let body = sanitize_action_field(reply);
            let mut action =
                format!("http, {label}, {publish_url}, method=POST, clear=true, body={body}");
            if let Some(token) = access_token {
                // `headers.Authorization=` is the ntfy action syntax for an
                // authenticated request; without it a protected topic 401s and
                // the tap does nothing.
                action.push_str(&format!(", headers.Authorization=Bearer {token}"));
            }
            action
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Escape the ntfy action-string separators and drop control characters.
///
/// The whole action set rides in one HTTP header, so a raw `\r`/`\n` makes
/// `reqwest` reject the header and the escalation is never delivered at all.
fn sanitize_action_field(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace(',', "\\,")
        .replace(';', "\\;")
}

#[cfg(test)]
mod action_header_tests {
    use super::*;

    #[test]
    fn uses_an_http_post_to_the_reply_topic_not_a_view_link() {
        // `view` only opens a URL; it never publishes, so the reply-topic
        // listener (which fires on a `message` event) would never see the tap
        // and the escalation would age into auto-deny.
        let h = ntfy_actions_header(
            "https://ntfy.sh",
            "agentos-reply",
            &[("\u{2705} Approve", "/approve 42")],
            None,
        );
        assert!(h.starts_with("http, "), "must be an http action, got: {h}");
        assert!(!h.contains("view,"), "got: {h}");
        assert!(h.contains("https://ntfy.sh/agentos-reply"), "got: {h}");
        assert!(h.contains("method=POST"), "got: {h}");
        // The published message must be the command verbatim — it is what
        // `InboundRouter::handle_approval_command` parses.
        assert!(h.contains("body=/approve 42"), "got: {h}");
    }

    #[test]
    fn a_protected_topic_gets_an_auth_header_on_the_action() {
        let h = ntfy_actions_header("https://n", "t", &[("A", "/approve 1")], Some("tok123"));
        assert!(
            h.contains("headers.Authorization=Bearer tok123"),
            "got: {h}"
        );
    }

    #[test]
    fn separators_are_escaped_and_control_chars_dropped() {
        // A raw newline makes reqwest reject the header outright, which would
        // drop the whole escalation rather than one button.
        let h = ntfy_actions_header(
            "https://n",
            "t",
            &[("yes, please; now\nX", "/approve 1")],
            None,
        );
        assert!(!h.contains('\n'), "control chars must not survive: {h:?}");
        assert!(h.contains(r"yes\, please\; nowX"), "got: {h}");
    }

    #[test]
    fn multiple_actions_are_semicolon_joined() {
        let h = ntfy_actions_header(
            "https://n",
            "t",
            &[("A", "/approve 1"), ("D", "/deny 1")],
            None,
        );
        assert_eq!(h.matches("http, ").count(), 2);
        assert!(h.contains("; http, D, "), "got: {h}");
    }
}
