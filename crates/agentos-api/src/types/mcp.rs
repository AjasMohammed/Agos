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
    /// When the attachment was persisted, if persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}
