use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Trust tier assigned to a tool manifest.
///
/// Determines the signature policy enforced by the kernel at load time:
/// - `Core`      — shipped with AgentOS, distribution-trusted (no runtime sig check).
/// - `Verified`  — community tool reviewed and co-signed by maintainers; author sig required.
/// - `Community` — author-signed only; user must opt-in to install.
/// - `Blocked`   — revoked; kernel hard-rejects even if locally installed.
///
/// Variant order is significant for trust comparisons: Core > Verified > Community > Blocked.
/// Note: derived Ord orders variants top-to-bottom (Core = 0, Blocked = 3), so
/// *lower numeric value = higher trust*. Use explicit comparisons: `tier <= TrustTier::Verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TrustTier {
    Core,
    Verified,
    #[default]
    Community,
    Blocked,
}

/// How the tool's logic is executed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExecutorType {
    #[default]
    Inline, // built-in Rust implementation (compiled into kernel)
    Wasm, // external .wasm module loaded at runtime
}

/// Executor configuration for a tool manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutor {
    #[serde(rename = "type", default)]
    pub executor_type: ExecutorType,
    /// Path to the .wasm file, relative to the manifest's directory.
    pub wasm_path: Option<PathBuf>,
}

impl Default for ToolExecutor {
    fn default() -> Self {
        Self {
            executor_type: ExecutorType::Inline,
            wasm_path: None,
        }
    }
}

/// A tool's manifest, parsed from tool.toml at install time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    pub manifest: ToolInfo,
    pub capabilities_required: ToolCapabilities,
    pub capabilities_provided: ToolOutputs,
    pub intent_schema: ToolSchema,
    /// Optional JSON Schema for validating the tool's input payload.
    /// When present, `SemanticPayload.data` is validated against this schema
    /// before the tool is executed.
    #[serde(default)]
    pub input_schema: Option<serde_json::Value>,
    pub sandbox: ToolSandbox,
    /// Which execution backend should run this tool. Defaults to Inline.
    #[serde(default)]
    pub executor: ToolExecutor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default)]
    pub checksum: Option<String>,
    /// Ed25519 public key of the tool author (hex-encoded, 64 chars).
    /// Required for `Verified` and `Community` trust tiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_pubkey: Option<String>,
    /// Ed25519 signature over the canonical signing payload (hex-encoded, 128 chars).
    /// Required for `Verified` and `Community` trust tiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Trust tier that controls how the kernel verifies this manifest.
    /// Defaults to `Community` if omitted.
    #[serde(default)]
    pub trust_tier: TrustTier,
    /// Searchable tags for marketplace discovery (e.g. ["github", "code-review"]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCapabilities {
    pub permissions: Vec<String>, // e.g. ["fs.read", "context.write"]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutputs {
    pub outputs: Vec<String>, // e.g. ["content.text", "content.structured"]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub input: String,  // e.g. "FileReadIntent"
    pub output: String, // e.g. "FileContent"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSandbox {
    pub network: bool,
    pub fs_write: bool,
    #[serde(default)]
    pub gpu: bool,
    pub max_memory_mb: u64,
    pub max_cpu_ms: u64,
    /// Explicit syscall allowlist override. Empty = use default base allowlist.
    #[serde(default)]
    pub syscalls: Vec<String>,
    /// Optional weight classification for sandbox resource allocation.
    /// Known values: "stateless", "memory", "network", "hal".
    /// Unknown values are preserved for forward compatibility and interpreted by
    /// higher layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<String>,
}

/// A registered tool in the kernel's tool registry.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    pub id: ToolID,
    pub manifest: ToolManifest,
    pub status: ToolStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Available,
    Running,
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::ToolSandbox;
    use serde_json::json;

    #[test]
    fn tool_sandbox_deserializes_without_weight() {
        let sandbox: ToolSandbox = serde_json::from_value(json!({
            "network": false,
            "fs_write": false,
            "gpu": false,
            "max_memory_mb": 64,
            "max_cpu_ms": 1_000,
            "syscalls": [],
        }))
        .unwrap();

        assert_eq!(sandbox.weight, None);
    }

    #[test]
    fn tool_sandbox_omits_absent_weight_when_serialized() {
        let sandbox = ToolSandbox {
            network: false,
            fs_write: false,
            gpu: false,
            max_memory_mb: 64,
            max_cpu_ms: 1_000,
            syscalls: vec![],
            weight: None,
        };

        let serialized = serde_json::to_value(&sandbox).unwrap();

        assert!(serialized.get("weight").is_none());
    }

    #[test]
    fn tool_sandbox_preserves_weight_when_present() {
        let sandbox: ToolSandbox = serde_json::from_value(json!({
            "network": false,
            "fs_write": false,
            "gpu": false,
            "max_memory_mb": 64,
            "max_cpu_ms": 1_000,
            "syscalls": [],
            "weight": "stateless",
        }))
        .unwrap();

        assert_eq!(sandbox.weight.as_deref(), Some("stateless"));

        let serialized = serde_json::to_value(&sandbox).unwrap();
        assert_eq!(
            serialized.get("weight").and_then(|v| v.as_str()),
            Some("stateless")
        );
    }
}
