use crate::tool::TrustTier;
use serde::{Deserialize, Serialize};

/// A plugin manifest loaded from `plugin.toml`.
///
/// Plugins are discovered from manifests alone (no code loaded at discovery time).
/// Activation loads and registers the plugin's tools, channels, and hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin ID (kebab-case, e.g. "discord", "memory-embeddings").
    pub id: String,
    /// Human-readable name.
    pub display_name: String,
    /// Plugin version (semver string).
    pub version: String,
    /// One-line description for listings.
    pub description: String,
    /// Optional author/organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Trust tier controlling signature requirements.
    #[serde(default)]
    pub trust_tier: TrustTier,
    /// Tool manifest file paths relative to this plugin manifest's directory.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Channels provided by this plugin.
    #[serde(default)]
    pub channels: Vec<ChannelDeclaration>,
    /// Whether this plugin provides a memory backend.
    #[serde(default)]
    pub memory_backend: bool,
    /// Permissions this plugin requires (e.g. "network.outbound", "fs.read").
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Ed25519 signature over canonical JSON (required for Community/Verified tier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Ed25519 public key of the signer (hex-encoded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_pubkey: Option<String>,
}

/// A channel declared by a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelDeclaration {
    /// Channel ID matching the `ChannelAdapter::id()` return value.
    pub id: String,
    /// Human-readable name shown in listings.
    pub display_name: String,
    /// Capability strings (e.g. "send", "receive", "presence", "reactions").
    #[serde(default)]
    pub capabilities: Vec<String>,
}
