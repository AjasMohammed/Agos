/// Microsoft Teams channel adapter.
///
/// Outbound: sends to an Incoming Webhook URL configured in Teams.
/// Inbound: receives via an HTTP webhook registered in the kernel's web server
///          at `/api/channels/teams` (wired separately in agentos-web).
use crate::types::{ChannelCapabilities, DeliveryReceipt, OutboundMessage};
use crate::{ChannelAdapter, ChannelHealth};
use agentos_http::{client, HttpProfile};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

pub struct TeamsAdapter {
    client: Client,
    /// The Incoming Webhook URL for outbound messages.
    webhook_url: Zeroizing<String>,
    name: String,
}

impl TeamsAdapter {
    pub fn new(webhook_url: String) -> Result<Self, AgentOSError> {
        crate::webhook::validate_webhook_url(&webhook_url)?;
        Ok(Self {
            client: client(HttpProfile::Outbound),
            webhook_url: Zeroizing::new(webhook_url),
            name: "teams".to_string(),
        })
    }
}

#[async_trait]
impl ChannelAdapter for TeamsAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: true,
            reactions: false,
            media: true,
            rich_formatting: true,
            max_message_length: 28_000,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let text = msg.content.as_text();

        let body = json!({
            "@type": "MessageCard",
            "@context": "https://schema.org/extensions",
            "text": text
        });

        let response = self
            .client
            .post(self.webhook_url.as_str())
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "teams".into(),
                reason: format!(
                    "HTTP request failed: {}",
                    crate::webhook::safe_reqwest_err(&e)
                ),
            })?;

        if !response.status().is_success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "teams".into(),
                reason: format!("Webhook returned HTTP {}", response.status()),
            });
        }

        Ok(DeliveryReceipt {
            message_id: Uuid::new_v4().to_string(),
            delivered_at: Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        _tx: mpsc::Sender<crate::types::InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        // Inbound messages arrive via HTTP webhook in agentos-web. No persistent connection.
        cancel.cancelled().await;
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        // Teams Incoming Webhook URLs embed the secret in the path — issuing a HEAD
        // request would leak the credential and is not a meaningful connectivity probe
        // (the endpoint only accepts POST). Report Connected passively; actual delivery
        // failures surface through send() errors.
        ChannelHealth::Connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_teams_capabilities() {
        let adapter = TeamsAdapter::new("https://example.com".to_string()).unwrap();
        let caps = adapter.capabilities();
        assert!(caps.rich_formatting);
        assert!(caps.threads);
    }

    #[test]
    fn test_teams_rejects_private_url() {
        assert!(TeamsAdapter::new("http://localhost/hook".to_string()).is_err());
        assert!(TeamsAdapter::new("https://192.168.1.1/hook".to_string()).is_err());
        assert!(TeamsAdapter::new("https://169.254.169.254/latest".to_string()).is_err());
    }

    #[test]
    fn test_teams_rejects_plain_http() {
        assert!(TeamsAdapter::new("http://example.com/hook".to_string()).is_err());
    }
}
