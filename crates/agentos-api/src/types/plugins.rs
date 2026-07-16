//! Plugin extensibility DTOs (Phase 05).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Summary row for a discovered/active plugin.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiPluginSummary {
    /// Unique plugin id (kebab-case).
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Semver version string.
    pub version: String,
    /// One-line description.
    pub description: String,
    /// Trust tier: `core` | `verified` | `community` | `blocked`.
    pub trust_tier: String,
    /// Lifecycle status: `discovered` | `active` | `disabled` | `blocked`.
    pub status: String,
    /// Populated only when `status == "blocked"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// Channel ids declared by this plugin.
    pub channels: Vec<String>,
    /// Tool manifest paths declared by this plugin.
    pub tools: Vec<String>,
}

/// Full plugin detail.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiPluginDetail {
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub trust_tier: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    pub channels: Vec<String>,
    pub tools: Vec<String>,
    pub permissions: Vec<String>,
    pub memory_backend: bool,
}

/// Acknowledgement returned by `POST /plugins/discover`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverPluginsResponse {
    /// Number of newly discovered plugins.
    pub discovered: u64,
    /// Full plugin inventory after discovery.
    pub plugins: Vec<ApiPluginSummary>,
}
