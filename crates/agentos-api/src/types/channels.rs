//! Channel extensibility DTOs (Phase 05).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Summary of a connected bidirectional channel instance.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiChannelSummary {
    /// Channel instance id (UUID).
    pub id: String,
    /// Channel kind (e.g. `telegram`, `slack`, `discord`).
    pub kind: String,
    /// Human-readable label.
    pub display_name: String,
    /// Channel-specific external identifier (chat id, topic, address).
    pub external_id: String,
    /// ntfy-specific reply topic, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_topic: Option<String>,
    /// ntfy-specific server URL, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    /// Telegram webhook URL, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    /// When the channel was connected.
    pub connected_at: DateTime<Utc>,
    /// Last inbound/outbound activity.
    pub last_active: DateTime<Utc>,
    /// Adapter health status, if a live adapter is running (`Healthy`, `Degraded`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
}
