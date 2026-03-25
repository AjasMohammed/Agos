use crate::ids::ChannelInstanceID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of external communication channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Telegram,
    Ntfy,
    Email,
    Custom(String),
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelKind::Telegram => write!(f, "telegram"),
            ChannelKind::Ntfy => write!(f, "ntfy"),
            ChannelKind::Email => write!(f, "email"),
            ChannelKind::Custom(s) => write!(f, "{s}"),
        }
    }
}

impl std::str::FromStr for ChannelKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "telegram" => Ok(ChannelKind::Telegram),
            "ntfy" => Ok(ChannelKind::Ntfy),
            "email" => Ok(ChannelKind::Email),
            other => Ok(ChannelKind::Custom(other.to_string())),
        }
    }
}

/// A connected external channel registered by the user.
///
/// Stores only non-sensitive routing metadata.  Credentials (bot tokens, passwords)
/// are stored in `agentos-vault` and referenced by name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredChannel {
    pub id: ChannelInstanceID,
    pub kind: ChannelKind,
    /// Channel-specific external identifier:
    /// - Telegram: chat_id as string
    /// - ntfy: topic name
    /// - email: to_address
    pub external_id: String,
    /// Human-readable label (e.g. "@johndoe", "john@example.com").
    pub display_name: String,
    /// Vault key for the bot token / auth credential (empty = no credential needed).
    #[serde(default)]
    pub credential_key: String,
    /// ntfy-specific: topic to subscribe to for inbound replies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_topic: Option<String>,
    /// ntfy-specific: base server URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    pub connected_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    pub active: bool,
}
