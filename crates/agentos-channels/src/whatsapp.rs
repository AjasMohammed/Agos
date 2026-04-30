use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_http::{client, HttpProfile};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub struct WhatsAppAdapter {
    access_token: Zeroizing<String>,
    pub phone_number_id: String,
    pub recipient_phone: String,
    pub instance_id: String,
    client: reqwest::Client,
}

impl WhatsAppAdapter {
    pub fn new(
        access_token: String,
        phone_number_id: String,
        recipient_phone: String,
        instance_id: String,
    ) -> Result<Self, AgentOSError> {
        // phone_number_id is interpolated into the URL path; reject anything
        // that is not purely numeric to prevent path traversal.
        if phone_number_id.is_empty() || !phone_number_id.chars().all(|c| c.is_ascii_digit()) {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "whatsapp".to_string(),
                reason: format!(
                    "invalid phone_number_id {:?}: must contain only ASCII digits",
                    phone_number_id
                ),
            });
        }
        Ok(Self {
            access_token: Zeroizing::new(access_token),
            phone_number_id,
            recipient_phone,
            instance_id,
            client: client(HttpProfile::Outbound),
        })
    }
}

#[async_trait]
impl ChannelAdapter for WhatsAppAdapter {
    fn name(&self) -> &str {
        "whatsapp"
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: false,
            reactions: false,
            media: true,
            rich_formatting: false,
            max_message_length: 4096,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let text = msg.content.as_text();
        let url = format!(
            "https://graph.facebook.com/v18.0/{}/messages",
            self.phone_number_id
        );
        let client = &self.client;
        let token = self.access_token.as_str();
        let recipient = &self.recipient_phone;
        let policy = crate::retry::RetryPolicy::default();

        crate::retry::with_retry(&policy, "whatsapp", || async {
            let resp = client
                .post(&url)
                .bearer_auth(token)
                .json(&serde_json::json!({
                    "messaging_product": "whatsapp",
                    "to": recipient,
                    "type": "text",
                    "text": {"body": &text}
                }))
                .send()
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "whatsapp".to_string(),
                    reason: format!(
                        "HTTP request failed: {}",
                        crate::webhook::safe_reqwest_err(&e)
                    ),
                })?;

            if !resp.status().is_success() {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "whatsapp".to_string(),
                    reason: format!("WhatsApp API error: HTTP {}", resp.status().as_u16()),
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
        _tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        // WhatsApp inbound is webhook-driven; the REST API layer handles it
        // and feeds messages to the ChannelManager directly.
        // This listener just parks until cancelled.
        cancel.cancelled().await;
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        let url = format!("https://graph.facebook.com/v18.0/{}", self.phone_number_id);
        match self
            .client
            .get(&url)
            .bearer_auth(self.access_token.as_str())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => ChannelHealth::Connected,
            Ok(r) => ChannelHealth::Degraded(format!("HTTP {}", r.status().as_u16())),
            Err(e) => ChannelHealth::Disconnected(crate::webhook::safe_reqwest_err(&e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phone_number_id_validation() {
        assert!(WhatsAppAdapter::new(
            "token".into(),
            "1234567890".into(),
            "+1555".into(),
            "inst".into()
        )
        .is_ok());

        // path traversal attempt
        assert!(WhatsAppAdapter::new(
            "token".into(),
            "123/messages".into(),
            "+1555".into(),
            "inst".into()
        )
        .is_err());

        // empty
        assert!(
            WhatsAppAdapter::new("token".into(), "".into(), "+1555".into(), "inst".into()).is_err()
        );
    }
}
