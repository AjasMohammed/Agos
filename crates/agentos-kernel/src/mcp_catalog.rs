//! MCP catalog — curated, installable MCP server entries.
//!
//! Each entry is a TOML file (`plugins/mcp-catalog/<id>.toml`) describing how to
//! install and attach a named MCP server. The catalog backs the planned
//! `agentos mcp catalog list/search/info` and `agentos mcp install <id>`
//! commands (release Phase 04). This module is the **parser + in-memory
//! registry**; the install command and CLI/bus wiring build on top of it.
//!
//! Trust: built-in (distribution-shipped) entries are trusted; user-supplied
//! `verified`-tier entries must carry a valid Ed25519 `author_pubkey` +
//! `signature` over the canonical entry payload — enforced by
//! [`CatalogRegistry::load`], which **demotes** unsigned or invalidly-signed
//! user entries to `community` (so `mcp install` then requires the explicit
//! `--unsafe-allow-community` opt-in). User entries claiming `core` are always
//! demoted: that tier is distribution-only.

use agentos_types::AgentOSError;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
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
    /// Hex-encoded Ed25519 public key of the entry author — required (with
    /// `signature`) for user-supplied `verified` entries.
    #[serde(default)]
    pub author_pubkey: Option<String>,
    /// Hex-encoded Ed25519 signature over [`signing_payload`] — required for
    /// user-supplied `verified` entries; built-in entries are
    /// distribution-trusted.
    #[serde(default)]
    pub signature: Option<String>,
    /// True for entries embedded in the binary (distribution-trusted, exempt
    /// from signature checks). Never read from TOML — set by the loader.
    #[serde(skip)]
    pub builtin: bool,
}

/// Build the deterministic signing payload for a catalog entry: a canonical
/// JSON object (BTreeMap-ordered keys) over the exec-relevant fields. Mutable
/// display metadata (`display_name`, `description`, `homepage`) and the
/// signature fields themselves are excluded.
pub fn signing_payload(entry: &CatalogEntry) -> Vec<u8> {
    let mut payload = Map::new();
    // Domain-separation tag: binds the signature to this payload schema, so a
    // signature can never be replayed against a future field layout.
    payload.insert("_scheme".into(), json!("agentos-mcp-catalog-v1"));
    payload.insert("args".into(), json!(entry.install.args));
    payload.insert("auth_credential".into(), json!(entry.auth.credential));
    payload.insert("auth_env".into(), json!(entry.auth.env));
    payload.insert("auth_kind".into(), json!(entry.auth.kind));
    payload.insert("id".into(), json!(entry.id));
    payload.insert(
        "min_runtime_version".into(),
        json!(entry.install.min_runtime_version),
    );
    payload.insert("package".into(), json!(entry.install.package));
    payload.insert("risk_class".into(), json!(entry.tools.default_risk_class));
    payload.insert("risk_overrides".into(), json!(entry.tools.overrides));
    payload.insert("runtime".into(), json!(entry.install.runtime));
    payload.insert("strategy".into(), json!(entry.install.strategy));
    payload.insert("transport".into(), json!(entry.mcp.transport));
    payload.insert("trust_tier".into(), json!(entry.trust_tier));
    payload.insert("url".into(), json!(entry.mcp.url));

    serde_json::to_vec(&Value::Object(payload))
        .expect("catalog signing payload serialization is infallible")
}

/// Verify the entry's Ed25519 `author_pubkey` + `signature` over the canonical
/// payload. Returns a human-readable reason on failure.
fn verify_entry_signature(entry: &CatalogEntry) -> Result<(), String> {
    let pubkey_hex = entry
        .author_pubkey
        .as_deref()
        .ok_or("missing author_pubkey")?;
    let sig_hex = entry.signature.as_deref().ok_or("missing signature")?;

    let pubkey_bytes: [u8; 32] = hex::decode(pubkey_hex)
        .map_err(|e| format!("author_pubkey is not valid hex: {e}"))?
        .try_into()
        .map_err(|_| "author_pubkey must be 32 bytes".to_string())?;
    let key = VerifyingKey::from_bytes(&pubkey_bytes)
        .map_err(|e| format!("invalid author_pubkey: {e}"))?;

    let sig_bytes: [u8; 64] = hex::decode(sig_hex)
        .map_err(|e| format!("signature is not valid hex: {e}"))?
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    let signature = Signature::from_bytes(&sig_bytes);

    key.verify(&signing_payload(entry), &signature)
        .map_err(|_| "signature does not match entry payload".to_string())
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
            let mut entry: CatalogEntry = toml::from_str(text)
                .map_err(|e| AgentOSError::CatalogParse(format!("embedded {file}: {e}")))?;
            entry.builtin = true;
            reg.insert_validated(entry)?;
        }
        if let Some(dir) = user_dir {
            let mut user = Self::load_from_dir(dir)?;
            user.enforce_user_trust_policy();
            reg.merge_override(user);
        }
        Ok(reg)
    }

    /// Trust policy for user-supplied (non-builtin) entries: `core` is
    /// distribution-only, and `verified` requires a valid Ed25519 signature.
    /// Violations are demoted to `community` (fail-closed: installing then
    /// requires the explicit `--unsafe-allow-community` opt-in) rather than
    /// dropped, so `catalog list` still shows the entry honestly.
    fn enforce_user_trust_policy(&mut self) {
        for entry in self.entries.values_mut() {
            if entry.builtin {
                continue;
            }
            match entry.trust_tier.as_str() {
                "core" => {
                    tracing::warn!(
                        catalog_id = %entry.id,
                        "User catalog entry claims 'core' tier (distribution-only) — demoting to 'community'"
                    );
                    entry.trust_tier = "community".to_string();
                }
                "verified" => {
                    if let Err(reason) = verify_entry_signature(entry) {
                        tracing::warn!(
                            catalog_id = %entry.id,
                            %reason,
                            "User catalog entry claims 'verified' tier without a valid Ed25519 signature — demoting to 'community'"
                        );
                        entry.trust_tier = "community".to_string();
                    }
                }
                _ => {}
            }
        }
    }

    /// Load every `*.toml` entry from `dir` (keyed by `entry.id`). A missing
    /// directory yields an empty registry (`Ok`). Invalid entries are rejected
    /// with a `CatalogParse` error rather than silently skipped.
    ///
    /// Crate-private on purpose: entries loaded here have NOT been through
    /// [`Self::enforce_user_trust_policy`] — external callers must go through
    /// [`Self::load`].
    pub(crate) fn load_from_dir(dir: &Path) -> Result<Self, AgentOSError> {
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

    /// Merge another registry, overriding by `id`. Built-in ids are protected:
    /// a user entry colliding with a distribution `builtin` entry is ignored
    /// (with a warning) so user catalogs cannot swap a trusted entry's exec
    /// details (command/package/args) under a name the operator trusts.
    pub fn merge_override(&mut self, other: CatalogRegistry) {
        for (id, e) in other.entries {
            if self
                .entries
                .get(&id)
                .is_some_and(|existing| existing.builtin)
            {
                tracing::warn!(
                    catalog_id = %id,
                    "user catalog entry shadows a built-in id — ignoring"
                );
                continue;
            }
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
            author_pubkey: None,
            signature: None,
            builtin: false,
        }
    }

    /// Sign `entry` with a deterministic test keypair, filling in
    /// `author_pubkey` + `signature`.
    fn sign(entry: &mut CatalogEntry) {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[7u8; 32]);
        entry.author_pubkey = Some(hex::encode(key.verifying_key().to_bytes()));
        let sig = key.sign(&signing_payload(entry));
        entry.signature = Some(hex::encode(sig.to_bytes()));
    }

    #[test]
    fn user_verified_entry_without_signature_is_demoted() {
        let mut reg = CatalogRegistry::default();
        reg.insert_validated(sample("unsigned")).unwrap();
        reg.enforce_user_trust_policy();
        assert_eq!(reg.lookup("unsigned").unwrap().trust_tier, "community");
    }

    #[test]
    fn user_verified_entry_with_valid_signature_is_kept() {
        let mut e = sample("signed");
        sign(&mut e);
        let mut reg = CatalogRegistry::default();
        reg.insert_validated(e).unwrap();
        reg.enforce_user_trust_policy();
        assert_eq!(reg.lookup("signed").unwrap().trust_tier, "verified");
    }

    #[test]
    fn user_entry_with_tampered_payload_is_demoted() {
        let mut e = sample("tampered");
        sign(&mut e);
        // Swap the package after signing — the exec-relevant payload changed.
        e.install.package = Some("evil-package".into());
        let mut reg = CatalogRegistry::default();
        reg.insert_validated(e).unwrap();
        reg.enforce_user_trust_policy();
        assert_eq!(reg.lookup("tampered").unwrap().trust_tier, "community");
    }

    #[test]
    fn user_core_claim_is_demoted_and_builtin_is_exempt() {
        let mut user_core = sample("fake-core");
        user_core.trust_tier = "core".into();
        let mut builtin = sample("real");
        builtin.builtin = true; // unsigned, but distribution-trusted
        let mut reg = CatalogRegistry::default();
        reg.insert_validated(user_core).unwrap();
        reg.insert_validated(builtin).unwrap();
        reg.enforce_user_trust_policy();
        assert_eq!(reg.lookup("fake-core").unwrap().trust_tier, "community");
        assert_eq!(reg.lookup("real").unwrap().trust_tier, "verified");
    }

    #[test]
    fn merge_override_refuses_to_shadow_builtin_ids() {
        let mut builtin = sample("filesystem");
        builtin.builtin = true;
        builtin.install.package = Some("trusted-package".into());
        let mut base = CatalogRegistry::default();
        base.insert_validated(builtin).unwrap();

        // User catalog tries to swap the trusted entry's exec details, and
        // also brings a legitimately new id.
        let mut shadow = sample("filesystem");
        shadow.install.package = Some("evil-package".into());
        let mut user = CatalogRegistry::default();
        user.insert_validated(shadow).unwrap();
        user.insert_validated(sample("brand-new")).unwrap();

        base.merge_override(user);
        assert_eq!(
            base.lookup("filesystem")
                .unwrap()
                .install
                .package
                .as_deref(),
            Some("trusted-package")
        );
        assert!(base.lookup("brand-new").is_some());
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
