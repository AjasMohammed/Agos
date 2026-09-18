//! MCP extensibility DTOs (Phase 05).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Call statistics for a running MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiMcpStats {
    pub total_calls: u64,
    pub failure_count: u32,
    pub avg_latency_ms: f64,
}

/// A running MCP server merged with its persisted attachment record.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiMcpServer {
    /// Logical server name (the attach key).
    pub name: String,
    /// Live supervisor state (`Connected`, `Connecting`, `Backoff`, `Stopped`, ...).
    /// `null` when only a persisted attachment exists with no live process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Permission that grants an agent every tool of this server
    /// (`mcp:<server>/:x`); pass it to the agent grant/revoke endpoints.
    pub permission: String,
    /// Number of tools exposed by the server.
    pub tool_count: usize,
    /// Call statistics (present only for live servers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<ApiMcpStats>,
    /// Optional supervisor note (e.g. reconnect attempt count).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Transport: `stdio` (command-based) or `http` (url-based).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// stdio command, if persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// stdio command args.
    pub args: Vec<String>,
    /// http URL, if persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Per-call timeout, if persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// OAuth connector id backing this server, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_connector_id: Option<String>,
    /// A static bearer token is stored for this server. The token itself is
    /// never returned — an edit that omits `auth_token` keeps it.
    pub has_auth_token: bool,
    /// Names of the persisted env vars. Values are withheld: they routinely
    /// hold secrets. An edit that omits `env` keeps them.
    pub env_keys: Vec<String>,
    /// When the attachment was persisted, if persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

/// Attach a new MCP server at runtime (mirrors `agentos mcp attach`).
/// Exactly one of `command` (stdio) or `url` (http) must be set.
#[derive(Deserialize, ToSchema)]
pub struct AttachMcpRequest {
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Static Bearer token for http transport; stored in the vault by the kernel.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub auth_token: Option<zeroize::Zeroizing<String>>,
    #[serde(default)]
    pub oauth_connector_id: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Subprocess env; `vault:KEY` values are resolved from the vault at attach
    /// time. On an edit, omit it to keep the stored env untouched.
    #[serde(default)]
    pub env: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpAttachedResponse {
    pub name: String,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiMcpCatalogEntry {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub trust_tier: String,
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// A server named after this catalog id is attached (live or persisted).
    pub installed: bool,
}

#[derive(Debug, Default, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct McpCatalogQuery {
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct InstallMcpRequest {
    #[serde(default)]
    pub allow_community: bool,
    #[serde(default)]
    pub no_auth: bool,
    #[serde(default)]
    pub runtime_binary: Option<String>,
}
