use agentos_types::{AgentOSError, WebhookProvider};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Verify a webhook signature based on the provider's conventions.
///
/// Returns `Ok(true)` if the signature is valid, `Ok(false)` if invalid,
/// and `Err` if the verification process itself fails (missing headers, etc.).
pub fn verify_webhook_signature(
    provider: &WebhookProvider,
    secret: &str,
    body: &[u8],
    headers: &std::collections::HashMap<String, String>,
) -> Result<bool, AgentOSError> {
    match provider {
        WebhookProvider::GitHub => verify_github(secret, body, headers),
        WebhookProvider::Stripe => verify_stripe(secret, body, headers),
        WebhookProvider::PagerDuty => verify_pagerduty(secret, body, headers),
        WebhookProvider::Generic => verify_generic(secret, body, headers),
        WebhookProvider::Slack => {
            // Slack verification is more complex (request signing with timestamp).
            // For now, fall back to generic HMAC-SHA256.
            verify_generic(secret, body, headers)
        }
        WebhookProvider::Custom {
            signature_header,
            algorithm,
        } => verify_custom(secret, body, headers, signature_header, algorithm),
    }
}

/// GitHub: `X-Hub-Signature-256: sha256=<hex>`
fn verify_github(
    secret: &str,
    body: &[u8],
    headers: &std::collections::HashMap<String, String>,
) -> Result<bool, AgentOSError> {
    let sig_header = headers.get("x-hub-signature-256").ok_or_else(|| {
        AgentOSError::SchemaValidation("Missing X-Hub-Signature-256 header".into())
    })?;

    let expected_hex = sig_header.strip_prefix("sha256=").ok_or_else(|| {
        AgentOSError::SchemaValidation("X-Hub-Signature-256 must start with 'sha256='".into())
    })?;

    verify_hmac_sha256(secret, body, expected_hex)
}

/// Stripe: `Stripe-Signature: t=<timestamp>,v1=<hex>`
///
/// Stripe signs `<timestamp>.<body>` with HMAC-SHA256.
fn verify_stripe(
    secret: &str,
    body: &[u8],
    headers: &std::collections::HashMap<String, String>,
) -> Result<bool, AgentOSError> {
    let sig_header = headers
        .get("stripe-signature")
        .ok_or_else(|| AgentOSError::SchemaValidation("Missing Stripe-Signature header".into()))?;

    // Parse "t=<ts>,v1=<hex>"
    let mut timestamp = None;
    let mut v1_sig = None;
    for part in sig_header.split(',') {
        let part = part.trim();
        if let Some(ts) = part.strip_prefix("t=") {
            timestamp = Some(ts);
        } else if let Some(sig) = part.strip_prefix("v1=") {
            v1_sig = Some(sig);
        }
    }

    let timestamp = timestamp
        .ok_or_else(|| AgentOSError::SchemaValidation("Missing 't=' in Stripe-Signature".into()))?;
    let expected_hex = v1_sig.ok_or_else(|| {
        AgentOSError::SchemaValidation("Missing 'v1=' in Stripe-Signature".into())
    })?;

    // Replay protection: reject events older than 5 minutes or more than 60s in the future
    let ts = timestamp.parse::<i64>().map_err(|_| {
        AgentOSError::SchemaValidation("Invalid timestamp in Stripe-Signature".into())
    })?;
    let age = chrono::Utc::now().timestamp() - ts;
    if !(-60..=300).contains(&age) {
        return Err(AgentOSError::SchemaValidation(format!(
            "Stripe webhook timestamp out of range ({age}s) — possible replay attack"
        )));
    }

    // Stripe signs: "{timestamp}.{body}"
    let signed_payload = format!("{timestamp}.{}", String::from_utf8_lossy(body));
    verify_hmac_sha256(secret, signed_payload.as_bytes(), expected_hex)
}

/// PagerDuty: `X-PagerDuty-Signature: v1=<hex>`
fn verify_pagerduty(
    secret: &str,
    body: &[u8],
    headers: &std::collections::HashMap<String, String>,
) -> Result<bool, AgentOSError> {
    let sig_header = headers.get("x-pagerduty-signature").ok_or_else(|| {
        AgentOSError::SchemaValidation("Missing X-PagerDuty-Signature header".into())
    })?;

    let expected_hex = sig_header.strip_prefix("v1=").ok_or_else(|| {
        AgentOSError::SchemaValidation("X-PagerDuty-Signature must start with 'v1='".into())
    })?;

    verify_hmac_sha256(secret, body, expected_hex)
}

/// Generic: `X-Signature: <hex>`
fn verify_generic(
    secret: &str,
    body: &[u8],
    headers: &std::collections::HashMap<String, String>,
) -> Result<bool, AgentOSError> {
    let sig_header = headers
        .get("x-signature")
        .ok_or_else(|| AgentOSError::SchemaValidation("Missing X-Signature header".into()))?;

    verify_hmac_sha256(secret, body, sig_header)
}

/// Custom provider with configurable header and algorithm.
fn verify_custom(
    secret: &str,
    body: &[u8],
    headers: &std::collections::HashMap<String, String>,
    signature_header: &str,
    algorithm: &agentos_types::SignatureAlgorithm,
) -> Result<bool, AgentOSError> {
    let header_key = signature_header.to_lowercase();
    let sig_value = headers.get(&header_key).ok_or_else(|| {
        AgentOSError::SchemaValidation(format!("Missing {signature_header} header"))
    })?;

    match algorithm {
        agentos_types::SignatureAlgorithm::HmacSha256 => {
            verify_hmac_sha256(secret, body, sig_value)
        }
        agentos_types::SignatureAlgorithm::HmacSha1 => {
            // We don't implement HMAC-SHA1 for now (deprecated).
            // Fall through to HMAC-SHA256.
            tracing::warn!("HMAC-SHA1 requested but falling back to HMAC-SHA256");
            verify_hmac_sha256(secret, body, sig_value)
        }
    }
}

/// Core HMAC-SHA256 verification with constant-time comparison.
fn verify_hmac_sha256(secret: &str, body: &[u8], expected_hex: &str) -> Result<bool, AgentOSError> {
    let expected_bytes = hex::decode(expected_hex)
        .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid hex in signature: {e}")))?;

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid HMAC key: {e}")))?;
    mac.update(body);

    // Constant-time comparison
    Ok(mac.verify_slice(&expected_bytes).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn compute_hmac_sha256(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn test_github_valid() {
        let secret = "test_secret";
        let body = b"hello world";
        let sig = compute_hmac_sha256(secret, body);

        let mut headers = HashMap::new();
        headers.insert("x-hub-signature-256".to_string(), format!("sha256={sig}"));

        assert!(
            verify_webhook_signature(&WebhookProvider::GitHub, secret, body, &headers).unwrap()
        );
    }

    #[test]
    fn test_github_invalid() {
        let secret = "test_secret";
        let body = b"hello world";

        let mut headers = HashMap::new();
        headers.insert(
            "x-hub-signature-256".to_string(),
            "sha256=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string(),
        );

        assert!(
            !verify_webhook_signature(&WebhookProvider::GitHub, secret, body, &headers).unwrap()
        );
    }

    #[test]
    fn test_github_missing_header() {
        let headers = HashMap::new();
        assert!(verify_webhook_signature(&WebhookProvider::GitHub, "s", b"b", &headers).is_err());
    }

    #[test]
    fn test_stripe_valid() {
        let secret = "whsec_test";
        let body = b"payload";
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let signed_payload = format!("{timestamp}.{}", String::from_utf8_lossy(body));
        let sig = compute_hmac_sha256(secret, signed_payload.as_bytes());

        let mut headers = HashMap::new();
        headers.insert(
            "stripe-signature".to_string(),
            format!("t={timestamp},v1={sig}"),
        );

        assert!(
            verify_webhook_signature(&WebhookProvider::Stripe, secret, body, &headers).unwrap()
        );
    }

    #[test]
    fn test_generic_valid() {
        let secret = "my_secret";
        let body = b"test body";
        let sig = compute_hmac_sha256(secret, body);

        let mut headers = HashMap::new();
        headers.insert("x-signature".to_string(), sig);

        assert!(
            verify_webhook_signature(&WebhookProvider::Generic, secret, body, &headers).unwrap()
        );
    }

    #[test]
    fn test_pagerduty_valid() {
        let secret = "pd_secret";
        let body = b"alert payload";
        let sig = compute_hmac_sha256(secret, body);

        let mut headers = HashMap::new();
        headers.insert("x-pagerduty-signature".to_string(), format!("v1={sig}"));

        assert!(
            verify_webhook_signature(&WebhookProvider::PagerDuty, secret, body, &headers).unwrap()
        );
    }

    #[test]
    fn test_wrong_secret_fails() {
        let body = b"payload";
        let sig = compute_hmac_sha256("correct_secret", body);

        let mut headers = HashMap::new();
        headers.insert("x-signature".to_string(), sig);

        assert!(!verify_webhook_signature(
            &WebhookProvider::Generic,
            "wrong_secret",
            body,
            &headers
        )
        .unwrap());
    }
}

/// Why an inbound provider webhook was refused. Both HTTP front-ends map this
/// to a status; the body stays generic so an anonymous caller learns nothing
/// about stored configuration.
#[derive(Debug)]
pub enum WebhookIngestError {
    /// Unknown, inactive, or malformed endpoint id.
    NotFound,
    RateLimited,
    /// The stored provider string no longer parses (config drift).
    UnknownProvider,
    InvalidSignature,
}

/// Single ingestion path for `POST /webhooks/incoming/{id}`: resolve the
/// endpoint, throttle, verify the provider signature, strip credential
/// headers, record receipt, enqueue for the owning agent. No side effect
/// happens before the signature check.
pub async fn ingest_webhook(
    registry: &crate::webhook_registry::WebhookRegistry,
    throttle: &crate::webhook_throttle::WebhookThrottle,
    batcher: &crate::webhook_batcher::WebhookBatcher,
    endpoint_id: &str,
    headers: std::collections::HashMap<String, String>,
    body: &[u8],
) -> Result<(), WebhookIngestError> {
    let endpoint_uuid: agentos_types::WebhookEndpointID = endpoint_id
        .parse()
        .map_err(|_| WebhookIngestError::NotFound)?;
    let (meta, secret) = match registry.get_endpoint_with_secret(&endpoint_uuid).await {
        Some((meta, secret)) if meta.active => (meta, secret),
        _ => return Err(WebhookIngestError::NotFound),
    };
    if !throttle.allow(&endpoint_uuid).await {
        tracing::warn!(endpoint_id, "Webhook rate-limited");
        return Err(WebhookIngestError::RateLimited);
    }
    let provider: WebhookProvider = serde_json::from_value(serde_json::json!(meta.provider))
        .map_err(|_| {
            tracing::error!(endpoint_id, provider = %meta.provider, "Unknown webhook provider");
            WebhookIngestError::UnknownProvider
        })?;
    let sig_valid =
        verify_webhook_signature(&provider, &secret, body, &headers).unwrap_or_else(|e| {
            tracing::warn!(endpoint_id, error = %e, "Signature verification error");
            false
        });
    if !sig_valid {
        tracing::warn!(endpoint_id, "Invalid webhook signature");
        return Err(WebhookIngestError::InvalidSignature);
    }
    let payload = serde_json::from_slice::<serde_json::Value>(body)
        .unwrap_or_else(|_| serde_json::json!({ "_raw": String::from_utf8_lossy(body) }));
    // Only provider event-metadata headers reach agents and the audit log;
    // anything that can carry a credential is dropped.
    let safe_headers: std::collections::HashMap<String, String> = headers
        .into_iter()
        .filter(|(name, _)| {
            name == "content-type"
                || name == "user-agent"
                || (name.starts_with("x-")
                    && !matches!(
                        name.as_str(),
                        "x-api-key"
                            | "x-auth-token"
                            | "x-access-token"
                            | "x-secret"
                            | "x-secret-key"
                            | "x-private-key"
                            | "x-password"
                            | "x-token"
                            | "x-authorization"
                    )
                    && !name.starts_with("x-auth-")
                    && !name.starts_with("x-secret-")
                    && !name.starts_with("x-access-"))
        })
        .collect();
    let event = agentos_types::WebhookEvent {
        endpoint_id: endpoint_uuid,
        provider: provider.clone(),
        headers: safe_headers,
        payload,
        received_at: chrono::Utc::now(),
        signature_valid: true,
    };
    if let Err(e) = registry.record_receipt(&endpoint_uuid).await {
        tracing::warn!(endpoint_id, error = %e, "Failed to record webhook receipt");
    }
    batcher
        .add_event(event, meta.agent_id, provider, meta.debounce_seconds)
        .await;
    tracing::debug!(endpoint_id, "Webhook received and enqueued");
    Ok(())
}
