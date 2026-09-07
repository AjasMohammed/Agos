//! Agent identity DTO (Phase 05).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Cryptographic identity of an agent (read-only).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiAgentIdentity {
    /// Agent id (UUID).
    pub id: String,
    /// Agent name.
    pub name: String,
    /// Ed25519 public key (hex), if the agent has an identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_hex: Option<String>,
    /// First 16 hex chars of the public key, as a short fingerprint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Lifecycle status (debug-rendered).
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}
