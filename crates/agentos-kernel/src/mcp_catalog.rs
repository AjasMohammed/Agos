//! MCP catalog — curated, installable MCP server entries.
//!
//! Each entry is a TOML file (`plugins/mcp-catalog/<id>.toml`) describing how to
//! install and attach a named MCP server. The catalog backs the planned
//! `agentos mcp catalog list/search/info` and `agentos mcp install <id>`
//! commands (release Phase 04). This module is the **parser + in-memory
//! registry**; the install command and CLI/bus wiring build on top of it.
//!
//! Trust: built-in (distribution-shipped) entries are trusted; user-supplied
//! `verified`-tier entries must carry an Ed25519 `signature` (checked by the
//! install path). `community` entries require an explicit opt-in to install.

use agentos_types::AgentOSError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Seed catalog entries embedded into the binary at build time, so
/// `agentos mcp catalog list` works with **no** files on disk (mirrors how the
/// CLI embeds `config/` and `skills/core/`). The folder path is relative to
/// this crate's manifest dir.
#[derive(rust_embed::RustEmbed)]
#[folder = "../../plugins/mcp-catalog/"]
struct EmbeddedCatalog;

/// One catalog entry — a curated, installable MCP server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogEntry {
    /// Stable identifier (lowercase kebab-case, no `/`, `\`, or `..`). This is
    /// the `install <id>` target and a filesystem-safe path component.
    pub id: String,
    pub display_name: String,
    pub description: String,
    /// `core` | `verified` | `community`. Community needs an explicit
    /// `--unsafe-allow-community` to install.
    #[serde(default = "default_trust_tier")]
    pub trust_tier: String,
    #[serde(default)]
    pub homepage: Option<String>,
    pub mcp: McpBlock,
    pub install: InstallBlock,
    #[serde(default)]
    pub auth: AuthBlock,
    #[serde(default)]
    pub tools: ToolsBlock,
    /// Ed25519 signature over the canonical entry — required for user-supplied
    /// `verified` entries; built-in entries are distribution-trusted.
    #[serde(default)]
    pub signature: Option<String>,
}

/// Transport block: how the kernel talks to the running server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpBlock {
    /// `stdio` | `http` | `sse`.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// For `http`/`sse` transports.
    #[serde(default)]
    pub url: Option<String>,
}

/// Install block: how to obtain/run the server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstallBlock {
    /// `npx` | `global` | `pip` | `bundled` | `prebuilt`.
    pub strategy: String,
    /// Package/spec, e.g. `@modelcontextprotocol/server-filesystem`.
    #[serde(default)]
    pub package: Option<String>,
    /// `node` | `python` — the runtime the resolver must satisfy.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Minimum runtime version (e.g. `"18"` for node).
    #[serde(default)]
    pub min_runtime_version: Option<String>,
    /// Default server args; `{home}` is expanded at install time.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Auth block: how the server authenticates.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthBlock {
    /// `none` | `api_key` | `oauth`.
    #[serde(default = "default_auth_type", rename = "type")]
    pub kind: String,
    /// Env var the server reads the token from (api_key).
    #[serde(default)]
    pub env: Option<String>,
    /// Credential reference, e.g. `vault:github_token`, resolved at attach.
    #[serde(default)]
    pub credential: Option<String>,
}

impl Default for AuthBlock {
    fn default() -> Self {
        Self {
            kind: default_auth_type(),
            env: None,
            credential: None,
        }
    }
}

/// Tools block: risk classification for this server's dynamic tools.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolsBlock {
    /// Default risk class for this server's dynamically-registered tools.
    /// Omitting `[tools]` preserves the adapter's `readonly_external` default.
    #[serde(default = "default_risk_class")]
    pub default_risk_class: String,
    /// Per-tool risk-class overrides (tool name → risk class), so write/exec
    /// tools (e.g. `write_file`) escalate through the ApprovalHook correctly.
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}

impl Default for ToolsBlock {
    fn default() -> Self {
        Self {
            default_risk_class: default_risk_class(),
            overrides: BTreeMap::new(),
        }
    }
}

fn default_trust_tier() -> String {
    "verified".to_string()
}
fn default_transport() -> String {
    "stdio".to_string()
}
fn default_auth_type() -> String {
    "none".to_string()
}
fn default_risk_class() -> String {
    "readonly_external".to_string()
}

/// In-memory MCP catalog: `id` → entry. Built-ins plus user entries (user
/// entries override built-ins by `id`).
#[derive(Debug, Clone, Default)]
pub struct CatalogRegistry {
    entries: BTreeMap<String, CatalogEntry>,
}

impl CatalogRegistry {
    /// Build the registry from the embedded seed entries, then overlay any
    /// user-supplied entries in `user_dir` (user entries override built-ins by
    /// `id`). This is the boot-time constructor — it works with no files on
    /// disk because the seeds ship inside the binary.
    pub fn load(user_dir: Option<&Path>) -> Result<Self, AgentOSError> {
        let mut reg = Self::default();
        for file in EmbeddedCatalog::iter() {
            let f = EmbeddedCatalog::get(&file).ok_or_else(|| {
                AgentOSError::CatalogParse(format!("embedded catalog read {file}"))
            })?;
            let text = std::str::from_utf8(&f.data)
                .map_err(|e| AgentOSError::CatalogParse(format!("embedded {file}: {e}")))?;
            let entry: CatalogEntry = toml::from_str(text)
                .map_err(|e| AgentOSError::CatalogParse(format!("embedded {file}: {e}")))?;
            reg.insert_validated(entry)?;
        }
        if let Some(dir) = user_dir {
            reg.merge_override(Self::load_from_dir(dir)?);
        }
        Ok(reg)
    }

    /// Load every `*.toml` entry from `dir` (keyed by `entry.id`). A missing
    /// directory yields an empty registry (`Ok`). Invalid entries are rejected
    /// with a `CatalogParse` error rather than silently skipped.
    pub fn load_from_dir(dir: &Path) -> Result<Self, AgentOSError> {
        let mut reg = Self::default();
        if !dir.exists() {
            return Ok(reg);
        }
        let rd = std::fs::read_dir(dir)
            .map_err(|e| AgentOSError::CatalogParse(format!("read dir {}: {e}", dir.display())))?;
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| AgentOSError::CatalogParse(format!("read {}: {e}", path.display())))?;
            let entry: CatalogEntry = toml::from_str(&text)
                .map_err(|e| AgentOSError::CatalogParse(format!("{}: {e}", path.display())))?;
            reg.insert_validated(entry)?;
        }
        Ok(reg)
    }

    /// Insert an entry after validating it (id charset/traversal, transport vs.
    /// install-strategy compatibility).
    pub fn insert_validated(&mut self, entry: CatalogEntry) -> Result<(), AgentOSError> {
        // id must be a safe, lowercase kebab-case path component.
        let id_ok = !entry.id.is_empty()
            && !entry.id.contains("..")
            && entry
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !id_ok {
            return Err(AgentOSError::CatalogParse(format!(
                "invalid catalog id '{}' (lowercase a-z0-9-, no path separators)",
                entry.id
            )));
        }
        // A local stdio server cannot be delivered as a prebuilt HTTP binary.
        if entry.mcp.transport == "stdio" && entry.install.strategy == "prebuilt" {
            return Err(AgentOSError::CatalogParse(format!(
                "entry '{}': stdio transport is incompatible with the prebuilt install strategy",
                entry.id
            )));
        }
        self.entries.insert(entry.id.clone(), entry);
        Ok(())
    }

    /// Merge another registry, overriding by `id` (user entries win over built-ins).
    pub fn merge_override(&mut self, other: CatalogRegistry) {
        for (id, e) in other.entries {
            self.entries.insert(id, e);
        }
    }

    pub fn lookup(&self, id: &str) -> Option<&CatalogEntry> {
        self.entries.get(id)
    }

    /// All entries, sorted by id (BTreeMap order).
    pub fn list(&self) -> Vec<&CatalogEntry> {
        self.entries.values().collect()
    }

    /// Case-insensitive substring search over id / display_name / description.
    pub fn search(&self, query: &str) -> Vec<&CatalogEntry> {
        let q = query.to_lowercase();
        self.entries
            .values()
            .filter(|e| {
                e.id.to_lowercase().contains(&q)
                    || e.display_name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            display_name: "X".into(),
            description: "a sample file server".into(),
            trust_tier: "verified".into(),
            homepage: None,
            mcp: McpBlock {
                transport: "stdio".into(),
                url: None,
            },
            install: InstallBlock {
                strategy: "npx".into(),
                package: None,
                runtime: None,
                min_runtime_version: None,
                args: vec![],
            },
            auth: AuthBlock::default(),
            tools: ToolsBlock::default(),
            signature: None,
        }
    }

    #[test]
    fn parses_entry_with_defaults() {
        let toml = r#"
id = "filesystem"
display_name = "Filesystem"
description = "Read/write files in allowed directories."
trust_tier = "verified"

[mcp]
transport = "stdio"

[install]
strategy = "npx"
package = "@modelcontextprotocol/server-filesystem"
runtime = "node"
min_runtime_version = "18"
"#;
        let e: CatalogEntry = toml::from_str(toml).unwrap();
        assert_eq!(e.id, "filesystem");
        assert_eq!(e.mcp.transport, "stdio");
        assert_eq!(e.auth.kind, "none"); // [auth] omitted → default
        assert_eq!(e.tools.default_risk_class, "readonly_external"); // [tools] omitted → default
    }

    #[test]
    fn registry_lookup_and_search() {
        let mut reg = CatalogRegistry::default();
        reg.insert_validated(sample("filesystem")).unwrap();
        reg.insert_validated(sample("github")).unwrap();
        assert_eq!(reg.len(), 2);
        assert!(reg.lookup("filesystem").is_some());
        assert!(reg.lookup("nope").is_none());
        assert_eq!(reg.search("github").len(), 1); // id match
        assert_eq!(reg.search("sample").len(), 2); // both descriptions contain "sample"
        assert_eq!(reg.search("nomatch").len(), 0);
    }

    #[test]
    fn rejects_traversal_id() {
        let mut reg = CatalogRegistry::default();
        assert!(reg.insert_validated(sample("../evil")).is_err());
        assert!(reg.insert_validated(sample("bad/slash")).is_err());
        assert!(reg.insert_validated(sample("UPPER")).is_err());
    }

    #[test]
    fn rejects_stdio_prebuilt_combo() {
        let mut reg = CatalogRegistry::default();
        let mut e = sample("weird");
        e.install.strategy = "prebuilt".into();
        assert!(reg.insert_validated(e).is_err());
    }

    #[test]
    fn load_embeds_seed_catalog_without_disk() {
        // Proves the seeds ship inside the binary: no `user_dir`, no disk reads.
        let reg = CatalogRegistry::load(None).expect("embedded catalog must load");
        for id in ["filesystem", "github", "sqlite"] {
            assert!(reg.lookup(id).is_some(), "missing embedded seed: {id}");
        }
    }

    #[test]
    fn loads_shipped_seed_catalog() {
        // The release-blocking seed entries must parse + validate against the
        // schema (guards drift between the TOML files and these structs).
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/mcp-catalog");
        let reg = CatalogRegistry::load_from_dir(&dir).expect("seed catalog must load");
        for id in ["filesystem", "github", "sqlite"] {
            assert!(reg.lookup(id).is_some(), "missing seed entry: {id}");
        }
        // github: api_key sourced from the vault.
        let gh = reg.lookup("github").unwrap();
        assert_eq!(gh.auth.kind, "api_key");
        assert_eq!(gh.auth.credential.as_deref(), Some("vault:github_token"));
        // sqlite: python runtime path.
        assert_eq!(
            reg.lookup("sqlite").unwrap().install.runtime.as_deref(),
            Some("python")
        );
    }
}
