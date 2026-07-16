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
}
