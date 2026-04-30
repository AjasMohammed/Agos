use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_http::{client, HttpProfile};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::IpAddr;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;

/// Strip the URL from a reqwest error before logging so credentials in
/// query strings cannot leak into logs or audit entries.
pub(crate) fn safe_reqwest_err(e: &reqwest::Error) -> String {
    if let Some(status) = e.status() {
        format!("HTTP {status}")
    } else if e.is_timeout() {
        "request timed out".to_string()
    } else if e.is_connect() {
        "connection failed".to_string()
    } else {
        "network error".to_string()
    }
}

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
            client: client(HttpProfile::Webhook),
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
        let client = &self.client;
        let target_url = &self.target_url;
        let policy = crate::retry::RetryPolicy::default();

        crate::retry::with_retry(&policy, "webhook", || async {
            let resp = client
                .post(target_url)
                .header("Content-Type", "application/json")
                .header("X-AgentOS-Signature", &signature)
                .body(body_bytes.clone())
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
        })
        .await
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

// ── URL validation helpers ────────────────────────────────────────────────────

/// Validate an outbound webhook URL. Requires HTTPS and blocks private IPs.
pub(crate) fn validate_webhook_url(raw: &str) -> Result<(), AgentOSError> {
    validate_url_inner(raw, true, "webhook")
}

/// Validate a channel server base URL (Matrix homeserver, Mattermost base URL).
/// Allows http or https; blocks private IPs to prevent SSRF.
pub(crate) fn validate_server_base_url(raw: &str, adapter: &str) -> Result<(), AgentOSError> {
    validate_url_inner(raw, false, adapter)
}

fn validate_url_inner(raw: &str, require_https: bool, name: &str) -> Result<(), AgentOSError> {
    let parsed = Url::parse(raw).map_err(|e| AgentOSError::ToolExecutionFailed {
        tool_name: name.to_string(),
        reason: format!("invalid URL: {e}"),
    })?;

    if require_https && parsed.scheme() != "https" {
        return Err(AgentOSError::ToolExecutionFailed {
            tool_name: name.to_string(),
            reason: format!("{name} URL must use HTTPS"),
        });
    }
    if !require_https && parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err(AgentOSError::ToolExecutionFailed {
            tool_name: name.to_string(),
            reason: format!(
                "server URL must use http or https, got '{}'",
                parsed.scheme()
            ),
        });
    }

    let host = parsed
        .host()
        .ok_or_else(|| AgentOSError::ToolExecutionFailed {
            tool_name: name.to_string(),
            reason: "URL must have a host".to_string(),
        })?;

    match host {
        url::Host::Domain(h) => {
            let h = h.to_lowercase();
            if h == "localhost"
                || h == "localhost."
                || h.ends_with(".local")
                || h.ends_with(".internal")
                || h.ends_with(".lan")
            {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: name.to_string(),
                    reason: "URL targets a private host".to_string(),
                });
            }
        }
        url::Host::Ipv4(v4) => {
            if is_private_ip(IpAddr::V4(v4)) {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: name.to_string(),
                    reason: "URL targets a private IP address".to_string(),
                });
            }
        }
        url::Host::Ipv6(v6) => {
            // Unmap IPv4-mapped addresses (::ffff:127.0.0.1) to catch SSRF via IPv6 form.
            let addr = if let Some(v4) = v6.to_ipv4_mapped() {
                IpAddr::V4(v4)
            } else {
                IpAddr::V6(v6)
            };
            if is_private_ip(addr) {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: name.to_string(),
                    reason: "URL targets a private IP address".to_string(),
                });
            }
        }
    }
    Ok(())
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_unspecified()
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 169 && o[1] == 254)
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

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
