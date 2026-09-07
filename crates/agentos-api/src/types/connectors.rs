//! Connector (OAuth) extensibility DTOs (Phase 05).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Summary of a registered connector and its connection status.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiConnectorSummary {
    /// Connector id.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether a stored OAuth credential exists for this connector.
    pub connected: bool,
    /// A connector manifest is registered (tools are callable once connected).
    pub registered: bool,
    /// An OAuth provider block exists in `oauth_providers.toml`, so "Connect" works.
    pub oauth_available: bool,
    /// OAuth provider name (from the stored credential), if connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Granted scopes (from the stored credential).
    pub scopes: Vec<String>,
    /// Token expiry (from the stored credential), if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Full connector detail.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiConnectorDetail {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub base_url: String,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Namespaced tool ids exposed by this connector (`<id>.<tool>`).
    pub tools: Vec<String>,
    /// Raw manifest TOML, when the connector was installed from a file the
    /// kernel owns — the panel prefills its edit form with this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_toml: Option<String>,
}

/// Register a connector from a pasted manifest TOML.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AddConnectorRequest {
    pub manifest_toml: String,
}

/// Response of `POST /connectors/{id}/oauth/start`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OAuthStartResponse {
    pub authorize_url: String,
}

/// Provider callback query (public route).
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct OAuthCallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Store an OAuth credential by hand (mirrors `agentos mcp oauth-store`).
#[derive(Deserialize, ToSchema)]
pub struct StoreCredentialRequest {
    #[serde(default)]
    pub provider: Option<String>,
    #[schema(value_type = String)]
    pub access_token: zeroize::Zeroizing<String>,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub refresh_token: Option<zeroize::Zeroizing<String>>,
    #[serde(default)]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub client_secret: Option<zeroize::Zeroizing<String>>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_in_secs: Option<i64>,
}
