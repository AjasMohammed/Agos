/// LINE Messaging API channel adapter.
///
/// Outbound: POST to LINE Reply API using a reply token from the inbound webhook.
/// Inbound: HTTP webhook at `/api/channels/line` in agentos-web, HMAC-SHA256 verified.
use crate::types::{ChannelCapabilities, DeliveryReceipt, InboundMessage, OutboundMessage};
use crate::{ChannelAdapter, ChannelHealth};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;
use zeroize::Zeroizing;

pub struct LineAdapter {
    client: Client,
    channel_access_token: Zeroizing<String>,
    /// Used to verify HMAC-SHA256 signatures on inbound webhooks.
    channel_secret: Zeroizing<String>,
    name: String,
}

impl LineAdapter {
    pub fn new(channel_access_token: String, channel_secret: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("HTTP client build failed"),
            channel_access_token: Zeroizing::new(channel_access_token),
            channel_secret: Zeroizing::new(channel_secret),
            name: "line".to_string(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.channel_access_token.as_str())
    }

    /// Verify the LINE webhook HMAC-SHA256 signature.
    ///
    /// LINE sends `X-Line-Signature: <base64(HMAC-SHA256(channel_secret, body))>`.
    /// We compute the expected MAC, base64-encode it, and compare in constant time.
    /// NOTE: LINE uses **base64** encoding, not hex. Using hex here will silently
    ///       reject every legitimate webhook.
    pub fn verify_signature(channel_secret: &[u8], body: &[u8], signature: &str) -> bool {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let Ok(mut mac) = HmacSha256::new_from_slice(channel_secret) else {
            return false;
        };
        mac.update(body);
        let expected =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        subtle::ConstantTimeEq::ct_eq(expected.as_bytes(), signature.as_bytes()).into()
    }

    /// Verify an inbound webhook request using the adapter's stored channel secret.
    pub fn verify_webhook(&self, body: &[u8], signature: &str) -> bool {
        Self::verify_signature(self.channel_secret.as_bytes(), body, signature)
    }
}

#[async_trait]
impl ChannelAdapter for LineAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: false,
            reactions: false,
            media: true,
            rich_formatting: false,
            max_message_length: 5_000,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        let text = msg.content.as_text();
        // channel_instance_id carries the LINE reply token for this request.
        let reply_token = &msg.channel_instance_id;

        let body = json!({
            "replyToken": reply_token,
            "messages": [{ "type": "text", "text": text }]
        });

        let response = self
            .client
            .post("https://api.line.me/v2/bot/message/reply")
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "line".into(),
                reason: format!("HTTP request failed: {e}"),
            })?;

        if !response.status().is_success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "line".into(),
                reason: format!("LINE API returned HTTP {}", response.status()),
            });
        }

        Ok(DeliveryReceipt {
            message_id: Uuid::new_v4().to_string(),
            delivered_at: Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        _tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        // LINE delivers inbound messages via webhook — no persistent connection needed.
        cancel.cancelled().await;
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        match self
            .client
            .get("https://api.line.me/v2/bot/info")
            .header("Authorization", self.auth_header())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => ChannelHealth::Connected,
            Ok(r) => ChannelHealth::Degraded(format!("HTTP {}", r.status())),
            Err(e) => {
                warn!(error = %e, "LINE health check failed");
                ChannelHealth::Disconnected(e.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_signature_uses_base64_not_hex() {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let secret = b"test_channel_secret";
        let body = b"hello world";

        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let expected_base64 =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        // Should pass with correct base64 signature
        assert!(LineAdapter::verify_signature(
            secret,
            body,
            &expected_base64
        ));

        // Should fail with hex encoding (wrong format)
        let mut mac2 = HmacSha256::new_from_slice(secret).unwrap();
        mac2.update(body);
        let hex_sig = hex::encode(mac2.finalize().into_bytes());
        assert!(!LineAdapter::verify_signature(secret, body, &hex_sig));
    }

    #[test]
    fn test_verify_signature_wrong_body_fails() {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let secret = b"test_channel_secret";
        let body = b"hello world";
        let wrong_body = b"different content";

        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert!(!LineAdapter::verify_signature(secret, wrong_body, &sig));
    }
}
