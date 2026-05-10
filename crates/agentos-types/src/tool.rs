use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// Privileged executor: runs OUTSIDE the bwrap sandbox via `pkexec` or a
    /// setuid helper. Reserved for tools that must elevate privilege (e.g.
    /// host package install). The kernel HARD-REJECTS unless the manifest
    /// is `trust_tier = "core"` AND `risk_class = "control_plane"` AND
    /// dispatch is gated by a resolved `PendingEscalation`.
    Privileged,
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

/// Classifies a tool's risk level for the interactive approval workflow.
///
/// Operations with higher risk require explicit human approval before execution.
/// The `ApprovalHook` checks this before every tool call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// Read-only access to local data (file read, memory search). Auto-approved.
    #[default]
    ReadonlyScoped,
    /// Read-only external access (web fetch, web search). Auto-approved.
    ReadonlyExternal,
    /// Write operations in the working directory (file write, create). Approval required.
    WriteScoped,
    /// Arbitrary shell command execution. Always requires approval.
    ExecCapable,
    /// Control plane operations (spawn agent, modify config). Always requires approval.
    ControlPlane,
    /// Interactive: the tool itself requires human input to proceed.
    Interactive,
}

impl RiskClass {
    /// Returns `true` when this risk class requires human approval before execution.
    pub fn requires_approval(&self) -> bool {
        matches!(
            self,
            RiskClass::WriteScoped
                | RiskClass::ExecCapable
                | RiskClass::ControlPlane
                | RiskClass::Interactive
        )
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
    /// Fallback chains: tried in order when the tool fails with a matching error category.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<FallbackRule>,
    /// Risk classification for the interactive approval workflow.
    /// High-risk operations (WriteScoped, ExecCapable, ControlPlane) trigger
    /// a human approval request before execution.
    #[serde(default)]
    pub risk_class: RiskClass,
    /// Hints for the LLM on when to use this tool and what to avoid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_hints: Option<UsageHints>,
    /// Coarse capability tags used by the L0/L1 paginated manual to filter and
    /// group tools (e.g. `["read", "fs"]`, `["write", "network"]`).
    /// Distinct from `manifest.tags` (free-form marketplace discovery) and
    /// `manifest.capability_tags` (semantic search vocabulary). Empty when
    /// not declared; the manual falls back to inferred category in that case.
    /// Recognised v1 taxonomy: `read`, `write`, `exec`, `network`, `fs`, `meta`.
    /// Unknown tags are preserved (forward-compat) but do not surface in filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Recognised v1 manifest tag taxonomy. Manifests may declare additional tags
/// for forward-compat; only these surface in pagination filters.
pub const MANIFEST_TAG_TAXONOMY_V1: &[&str] = &["read", "write", "exec", "network", "fs", "meta"];

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UsageHints {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub use_for: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer_over: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quick_example: Option<serde_json::Value>,
}

/// A single fallback rule in a tool manifest's degradation chain.
///
/// When the tool fails with an error whose `error_category()` matches `on_error`,
/// the kernel retries with `try_tool` after applying `transform` to the payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackRule {
    /// Error category to match (e.g., "StorageError", "PermissionDenied").
    pub on_error: String,
    /// Tool to try as fallback.
    pub try_tool: String,
    /// Payload key transformations (key → "op:value", e.g., "prepend:/tmp/").
    #[serde(default)]
    pub transform: HashMap<String, String>,
    /// Maximum number of times to retry this specific fallback before moving to the
    /// next rule in the chain. Reserved for future use — the current kernel resolver
    /// treats the chain depth as the retry mechanism (each entry in `fallbacks` is one
    /// attempt). This field is preserved for forward compatibility with manifests that
    /// declare it, but is not yet evaluated at runtime.
    #[serde(default = "default_max_retries")]
    pub max_retries: u8,
}

fn default_max_retries() -> u8 {
    1
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
    /// Semantic capability tags for agent discoverability.
    /// Embedded alongside the description for intent-based tool search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_tags: Vec<String>,
    /// Tool-selector partition (e.g. fs, network). Empty when uncategorized.
    #[serde(default)]
    pub group: String,
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
    fn tool_manifest_tags_default_empty() {
        use crate::tool::ToolManifest;
        let toml = r#"
[manifest]
name = "x"
version = "1.0"
description = "desc"
author = "test"
trust_tier = "core"

[capabilities_required]
permissions = []

[capabilities_provided]
outputs = []

[intent_schema]
input  = "x"
output = "y"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 8
max_cpu_ms    = 500
syscalls      = []
        "#;
        let m: ToolManifest = toml::from_str(toml).unwrap();
        assert!(m.tags.is_empty());
    }

    #[test]
    fn tool_manifest_tags_round_trip() {
        use crate::tool::ToolManifest;
        let toml = r#"
tags = ["read", "fs"]

[manifest]
name = "x"
version = "1.0"
description = "desc"
author = "test"
trust_tier = "core"

[capabilities_required]
permissions = []

[capabilities_provided]
outputs = []

[intent_schema]
input  = "x"
output = "y"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 8
max_cpu_ms    = 500
syscalls      = []
        "#;
        let m: ToolManifest = toml::from_str(toml).unwrap();
        assert_eq!(m.tags, vec!["read".to_string(), "fs".to_string()]);
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
