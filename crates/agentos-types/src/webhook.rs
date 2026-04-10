use crate::ids::{AgentID, WebhookEndpointID};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A registered webhook endpoint that external services can POST to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    pub id: WebhookEndpointID,
    pub agent_id: AgentID,
    pub provider: WebhookProvider,
    /// HMAC secret for signature verification (not stored in this struct — kept in registry).
    #[serde(skip)]
    pub secret: String,
    pub debounce_seconds: u64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub last_received_at: Option<DateTime<Utc>>,
    pub total_received: u64,
}

/// Supported webhook provider types.
///
/// Each provider has different signature verification conventions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WebhookProvider {
    /// GitHub: `X-Hub-Signature-256: sha256=<hex>`
    GitHub,
    /// Stripe: `Stripe-Signature: t=<ts>,v1=<hex>`
    Stripe,
    /// Slack: request body contains a `token` field (legacy) or uses signing secret
    Slack,
    /// PagerDuty: `X-PagerDuty-Signature: v1=<hex>`
    PagerDuty,
    /// Generic HMAC-SHA256: `X-Signature: <hex>`
    Generic,
    /// Custom provider with configurable signature header and algorithm
    Custom {
        signature_header: String,
        algorithm: SignatureAlgorithm,
    },
}

impl std::fmt::Display for WebhookProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitHub => write!(f, "github"),
            Self::Stripe => write!(f, "stripe"),
            Self::Slack => write!(f, "slack"),
            Self::PagerDuty => write!(f, "pagerduty"),
            Self::Generic => write!(f, "generic"),
            Self::Custom { .. } => write!(f, "custom"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgorithm {
    HmacSha256,
    HmacSha1,
}

/// A received webhook event, normalized for internal processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEvent {
    pub endpoint_id: WebhookEndpointID,
    pub provider: WebhookProvider,
    pub headers: HashMap<String, String>,
    pub payload: serde_json::Value,
    pub received_at: DateTime<Utc>,
    pub signature_valid: bool,
}

/// Metadata about a webhook endpoint (no secrets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpointMeta {
    pub id: WebhookEndpointID,
    pub agent_id: AgentID,
    pub provider: String,
    pub debounce_seconds: u64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub last_received_at: Option<DateTime<Utc>>,
    pub total_received: u64,
}
