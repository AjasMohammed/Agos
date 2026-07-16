//! Webhook endpoint extensibility DTOs (Phase 05).
//!
//! Note: the secret is returned exactly once on create/rotate (in
//! [`WebhookSecretResponse`]). It is never stored in a way the API can read back,
//! so list/detail never expose it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A registered inbound webhook endpoint (no secret material).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiWebhookEndpoint {
    /// Endpoint id (UUID) — used in the ingress URL.
    pub id: String,
    /// Owning agent id (UUID).
    pub agent_id: String,
    /// Provider (JSON-encoded variant string, e.g. `"github"`).
    pub provider: String,
    /// Whether the endpoint is active.
    pub active: bool,
    /// Debounce window in seconds.
    pub debounce_seconds: u64,
    /// Total events received.
    pub total_received: u64,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_received_at: Option<DateTime<Utc>>,
    /// Convenience: the ingress path for this endpoint.
    pub inbound_url: String,
}

/// Create a new webhook endpoint for an agent.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateWebhookRequest {
    /// Name of the owning agent.
    pub agent_name: String,
    /// Provider: `github` | `stripe` | `slack` | `pagerduty` | `generic`.
    pub provider: String,
    /// Optional debounce window in seconds (default 0).
    #[serde(default)]
    pub debounce_seconds: Option<u64>,
}

/// One-shot secret response, returned only on create/rotate.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WebhookSecretResponse {
    /// Endpoint id.
    pub id: String,
    /// The HMAC secret — shown once; store it securely.
    pub secret: String,
    /// Ingress path for this endpoint.
    pub inbound_url: String,
}
