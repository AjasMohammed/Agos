use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
pub struct TokenRequest {
    pub api_key: Zeroizing<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: Zeroizing<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: Zeroizing<String>,
    pub refresh_token: Zeroizing<String>,
    pub expires_in: u64,
    pub token_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    pub name: String,
    pub permissions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

// ── React control panel: browser auth & key management (Phase 01) ──────────

/// Operator login. The `credential` is verified (constant-time) against the
/// configured operator token (`[web] auth_token`); on success a scoped,
/// expiring API key is minted and returned once.
#[derive(Clone, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// The operator credential (deployment auth token).
    pub credential: String,
}

/// A newly minted API key, returned exactly once at login / key creation.
/// The raw `api_key` is never retrievable again.
#[derive(Clone, Serialize, ToSchema)]
pub struct IssuedKeyResponse {
    /// The full `agos_<key>` secret — shown once; store it securely.
    pub api_key: String,
    /// The public, non-secret key id (use for `DELETE /keys/{id}`).
    pub key_id: String,
    /// Display name of the key.
    pub name: String,
    /// Permission scopes granted to the key.
    pub scopes: Vec<String>,
    /// Expiry timestamp, if the key is time-limited.
    pub expires_at: Option<DateTime<Utc>>,
}

/// Identity of the presented key — lets the SPA render scopes and gate UI.
#[derive(Clone, Serialize, ToSchema)]
pub struct AuthMe {
    pub key_id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Management metadata for an API key (never includes key material).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiKeyMeta {
    /// Public, non-secret key id.
    pub key_id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

/// Request to mint a new API key from the management surface (`keys:rw`).
#[derive(Clone, Deserialize, ToSchema)]
pub struct CreateKeyRequest {
    /// Display name for the new key.
    pub name: String,
    /// Permission scopes to grant (e.g. `["agents:r", "tasks:rw"]`).
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional time-to-live in seconds. Omit for a non-expiring key.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}
