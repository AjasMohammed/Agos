use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_http::{client, HttpProfile};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use zeroize::Zeroizing;

/// Build inbound content from a Slack message: text body + `files[]`. Image
/// files (by `mimetype`) become `MessageContent::Image`, others `File`; the
/// `url_private` URL requires the bot token to download, which the kernel
/// supplies (gated to `slack.com`). Returns `None` when there's nothing to send.
fn slack_message_content(m: &serde_json::Value) -> Option<MessageContent> {
    let text = m["text"].as_str().unwrap_or("");
    let mut media: Vec<MessageContent> = Vec::new();
    if let Some(files) = m["files"].as_array() {
        for f in files {
            let url = match f["url_private"].as_str() {
                Some(u) if !u.is_empty() => u.to_string(),
                _ => continue,
            };
            let filename = f["name"].as_str().unwrap_or("file").to_string();
            let mime = f["mimetype"].as_str().unwrap_or("");
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
            client: client(HttpProfile::Outbound),
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
        let text: String = msg
            .content
            .render_for_delivery()
            .chars()
            .take(40_000)
            .collect();
        let thread_ts = msg.thread_id.clone();
        let client = &self.client;
        let token = self.bot_token.as_str();
        let channel = &self.channel_id;
        let policy = crate::retry::RetryPolicy::default();

        crate::retry::with_retry(&policy, "slack", || async {
            let mut body = serde_json::json!({
                "channel": channel,
                "text": &text
            });
            if let Some(ts) = thread_ts.as_deref().filter(|s| !s.trim().is_empty()) {
                body["thread_ts"] = serde_json::Value::String(ts.to_string());
            }
            let resp = client
                .post("https://slack.com/api/chat.postMessage")
                .bearer_auth(token)
                .json(&body)
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
        })
        .await
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
                                    // Skip bot-authored messages (incl. our own posts) to
                                    // avoid echo loops; advance the cursor first so they
                                    // aren't re-fetched.
                                    if slack_msg["bot_id"].as_str().is_some() {
                                        continue;
                                    }
                                    if let Some(content) = slack_message_content(slack_msg) {
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
                                            content,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_only() {
        let m = json!({ "text": "hello" });
        assert!(matches!(
            slack_message_content(&m),
            Some(MessageContent::Text(t)) if t == "hello"
        ));
    }

    #[test]
    fn empty_is_none() {
        let m = json!({ "text": "" });
        assert!(slack_message_content(&m).is_none());
    }

    #[test]
    fn image_file_becomes_image() {
        let m = json!({
            "text": "",
            "files": [
                { "url_private": "https://files.slack.com/a/cat.png", "name": "cat.png", "mimetype": "image/png" }
            ]
        });
        match slack_message_content(&m) {
            Some(MessageContent::Image { url, .. }) => {
                assert_eq!(url, "https://files.slack.com/a/cat.png");
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn text_plus_file_becomes_mixed() {
        let m = json!({
            "text": "see attached",
            "files": [
                { "url_private": "https://files.slack.com/a/r.pdf", "name": "r.pdf", "mimetype": "application/pdf" }
            ]
        });
        match slack_message_content(&m) {
            Some(MessageContent::Mixed(parts)) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[0], MessageContent::Text(t) if t == "see attached"));
                assert!(matches!(&parts[1], MessageContent::File { .. }));
            }
            other => panic!("expected Mixed, got {other:?}"),
        }
    }

    #[test]
    fn file_without_url_private_skipped() {
        let m = json!({ "text": "", "files": [ { "name": "x" } ] });
        assert!(slack_message_content(&m).is_none());
    }
}
