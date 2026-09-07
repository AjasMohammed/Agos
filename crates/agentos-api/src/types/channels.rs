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
    /// Default agent for inbound chat on this channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_agent_name: Option<String>,
}

/// Register a bidirectional channel (mirrors `agentos channel connect`).
#[derive(Deserialize, ToSchema)]
pub struct ConnectChannelRequest {
    /// telegram | ntfy | email | discord | slack | whatsapp | webhook
    pub kind: String,
    pub display_name: String,
    /// Telegram chat_id, ntfy topic, email address. Optional for Telegram.
    #[serde(default)]
    pub external_id: Option<String>,
    /// Vault key holding the bot token / password.
    #[serde(default)]
    pub credential_key: Option<String>,
    /// Inline credential; stored in the vault under `credential_key`
    /// (or `channel.<kind>.<display-name-slug>` when that is empty) before connecting.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub credential: Option<zeroize::Zeroizing<String>>,
    #[serde(default)]
    pub reply_topic: Option<String>,
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub active_agent_name: Option<String>,
}

/// Edit a connected channel in place (`PUT /channels/{id}`).
///
/// Three-state per field: omitted leaves it unchanged, `""` clears it, a value
/// sets it. `kind` is not editable — a different kind is a different adapter,
/// so that is a disconnect + connect.
#[derive(Default, Deserialize, ToSchema)]
pub struct UpdateChannelRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    /// Vault key holding the credential. Omit to keep the current one.
    #[serde(default)]
    pub credential_key: Option<String>,
    /// New inline credential, stored under the channel's existing vault key.
    /// Omit to keep the stored secret untouched.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub credential: Option<zeroize::Zeroizing<String>>,
    #[serde(default)]
    pub reply_topic: Option<String>,
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub active_agent_name: Option<String>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct SetChannelAgentRequest {
    /// Agent name; omit or empty to clear the default.
    #[serde(default)]
    pub agent_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiPairingEntry {
    pub channel_id: String,
    pub sender_id: String,
    pub approved_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiPendingPairing {
    pub channel_id: String,
    pub sender_id: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiPairings {
    pub approved: Vec<ApiPairingEntry>,
    pub pending: Vec<ApiPendingPairing>,
}
