use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;

pub struct WebhookAdapter {
    pub target_url: String,
    secret: Zeroizing<String>,
    pub instance_id: String,
    client: reqwest::Client,
}

impl WebhookAdapter {
    pub fn new(target_url: String, secret: String, instance_id: String) -> Self {
        Self {
            target_url,
            secret: Zeroizing::new(secret),
            instance_id,
            client: reqwest::Client::new(),
        }
    }

    pub fn sign(&self, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .expect("HMAC can take any key length");
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    pub fn verify_signature(&self, body: &[u8], sig: &str) -> bool {
        let expected = self.sign(body);
        expected.as_bytes().ct_eq(sig.as_bytes()).into()
    }
}

#[async_trait]
impl ChannelAdapter for WebhookAdapter {
    fn name(&self) -> &str {
        "webhook"
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: false,
            reactions: false,
            media: false,
            rich_formatting: false,
            max_message_length: 100_000,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let body = serde_json::json!({
            "channel_instance_id": msg.channel_instance_id,
            "content": msg.content,
            "thread_id": msg.thread_id,
        });
        let body_bytes =
            serde_json::to_vec(&body).map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "webhook".to_string(),
                reason: e.to_string(),
            })?;
        let signature = self.sign(&body_bytes);

        let resp = self
            .client
            .post(&self.target_url)
            .header("Content-Type", "application/json")
            .header("X-AgentOS-Signature", signature)
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "webhook".to_string(),
                reason: e.to_string(),
            })?;

        if !resp.status().is_success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "webhook".to_string(),
                reason: format!("Webhook delivery failed: {}", resp.status()),
            });
        }

        Ok(DeliveryReceipt {
            message_id: uuid::Uuid::new_v4().to_string(),
            delivered_at: chrono::Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        _tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        // Inbound webhooks arrive via the REST API layer (agentos-api)
        // which forwards them to ChannelManager::inbound_tx directly.
        cancel.cancelled().await;
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        match self.client.head(&self.target_url).send().await {
            Ok(r) if r.status().is_success() || r.status().as_u16() == 405 => {
                ChannelHealth::Connected
            }
            Ok(r) => ChannelHealth::Degraded(format!("status {}", r.status())),
            Err(e) => ChannelHealth::Disconnected(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhook_sign_and_verify() {
        let adapter = WebhookAdapter::new(
            "https://example.com/webhook".to_string(),
            "my-secret".to_string(),
            "test-instance".to_string(),
        );
        let body = b"hello world";
        let sig = adapter.sign(body);
        assert!(adapter.verify_signature(body, &sig));
        assert!(!adapter.verify_signature(b"tampered", &sig));
    }
}
