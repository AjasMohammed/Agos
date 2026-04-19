---
title: Phase 2 — MCP Catalog Format & Registry
tags:
  - kernel
  - mcp
  - catalog
  - phase-2
date: 2026-04-18
status: planned
effort: 1.5d
priority: high
---

# Phase 2 — MCP Catalog Format & Registry

> Define the `*.toml` schema for catalog entries and build the registry that loads them from the embedded built-ins plus the user-local directory.

---

## Why this phase

`agentos mcp install <name>` needs a catalog to look up `<name>` against. The catalog entry drives every subsequent step: runtime selection, package prefetch, OAuth helper, risk classes, final attach command.

The format must be:
- **TOML**, to match `plugin.toml`, `SKILL.toml`, and `tools/*/*.toml` elsewhere.
- **Embeddable**, so built-in entries ship inside the release binary (no extra install steps).
- **User-extensible**, so operators can author private entries without rebuilding AgentOS.
- **Validated**, so typos in `binary` or `runtime_min_version` fail loudly.

---

## Current → Target

**Current:** No catalog exists. `plugins/core/*/plugin.toml` describes channel plugins only.

**Target:** Two new locations + a registry:

```
plugins/mcp-catalog/              # embedded via rust-embed (built-in)
  gmail.toml
  github.toml
  filesystem.toml
  …

~/.agentos/mcp-catalog/           # user-local, hot-reloadable
  my-private-server.toml
```

Plus `crates/agentos-kernel/src/mcp_catalog.rs` exposing `CatalogRegistry::lookup(name)`.

---

## Catalog entry schema

```toml
# plugins/mcp-catalog/gmail.toml

id = "gmail"                                     # unique catalog key
display_name = "Gmail"
version = "1.0.0"                                # catalog entry version
description = "Read, send, search emails + manage labels and filters"
homepage = "https://github.com/GongRzhe/Gmail-MCP-Server"
trust_tier = "verified"                          # core | verified | community
# signature = "…"  # Ed25519 over canonical JSON payload (required for verified)
# author_pubkey = "…"

[mcp]
transport = "stdio"                              # "stdio" | "http"
runtime = "node"                                 # resolver key; omit if runtime is irrelevant
runtime_min_version = "18"
timeout_secs = 30
auto_reconnect = true
max_response_bytes = 1048576                     # 1 MB default
rate_limit_rpm = 60

[mcp.install]
strategy = "npx"                                 # "npx" | "global" | "pip" | "bundled" | "prebuilt"
package = "@gongrzhe/server-gmail-autoauth-mcp"  # npm package or pypi name
package_version = "1.1.11"                       # pin; "*" to use latest
entry_js = "dist/index.js"                       # for strategy=npx/global with node: the file the resolver runs
# binary = "gmail-mcp"                           # alternative: invoke a pre-installed binary by name (resolver finds it)
prefetch_timeout_secs = 300                      # first-run npx can take minutes

[[mcp.install.args]]                             # args passed to the package entry (post-resolution)
value = "--stdio"

[mcp.env]
# Static env vars (same format as mcp attach --env). Values may use vault: refs.
GMAIL_OAUTH_PATH = "{home}/.gmail-mcp/gcp-oauth.keys.json"
GMAIL_CREDENTIALS_PATH = "{home}/.gmail-mcp/credentials.json"

[mcp.auth]
type = "oauth"                                   # "oauth" | "api_key" | "app_password" | "none"
provider = "google"
helper_command = "npx"
helper_args = ["-y", "@gongrzhe/server-gmail-autoauth-mcp", "auth"]
credentials_path = "{home}/.gmail-mcp/credentials.json"  # kernel checks this before attach
helper_requires_browser = true

[mcp.tools]
# Risk-class overrides; default for MCP tools is "exec_capable"
default_risk_class = "exec_capable"

[mcp.tools.overrides]
search_emails = "readonly_external"
read_email = "readonly_external"
list_email_labels = "readonly_external"
get_or_create_label = "write_scoped"
send_email = "exec_capable"
delete_email = "exec_capable"
batch_delete_emails = "exec_capable"
```

Alternative — **HTTP transport, no runtime needed:**

```toml
# plugins/mcp-catalog/hosted-example.toml
id = "hosted-example"
display_name = "Hosted Example"
version = "1.0.0"
trust_tier = "verified"

[mcp]
transport = "http"
timeout_secs = 30

[mcp.install]
strategy = "prebuilt"                            # nothing to install; the server is remote
url = "https://mcp.example.com/endpoint"

[mcp.auth]
type = "api_key"
env_var = "EXAMPLE_API_KEY"                      # user prompted to supply; stored in vault
```

---

## Rust types

**File:** `crates/agentos-kernel/src/mcp_catalog.rs` (new)

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use agentos_types::TrustTier;

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub display_name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    pub trust_tier: TrustTier,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub author_pubkey: Option<String>,
    pub mcp: McpBlock,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpBlock {
    pub transport: McpTransportKind,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub runtime_min_version: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_auto_reconnect")]
    pub auto_reconnect: bool,
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
    #[serde(default = "default_rate_limit_rpm")]
    pub rate_limit_rpm: u32,
    pub install: InstallBlock,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub auth: Option<AuthBlock>,
    #[serde(default)]
    pub tools: Option<ToolsBlock>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportKind {
    Stdio,
    Http,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum InstallBlock {
    Npx {
        package: String,
        #[serde(default = "star")]
        package_version: String,
        entry_js: String,
        #[serde(default)]
        args: Vec<InstallArg>,
        #[serde(default = "default_prefetch_timeout")]
        prefetch_timeout_secs: u64,
    },
    Global {
        package: String,
        #[serde(default = "star")]
        package_version: String,
        binary: String,
        #[serde(default)]
        args: Vec<InstallArg>,
    },
    Pip {
        package: String,
        #[serde(default = "star")]
        package_version: String,
        module: String,
        #[serde(default)]
        args: Vec<InstallArg>,
    },
    Bundled {
        path: PathBuf,
        #[serde(default)]
        args: Vec<InstallArg>,
    },
    Prebuilt {
        url: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallArg {
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthBlock {
    #[serde(rename = "type")]
    pub kind: AuthKind,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub helper_command: Option<String>,
    #[serde(default)]
    pub helper_args: Vec<String>,
    #[serde(default)]
    pub credentials_path: Option<String>,
    #[serde(default)]
    pub helper_requires_browser: bool,
    #[serde(default)]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    Oauth,
    ApiKey,
    AppPassword,
    None,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolsBlock {
    #[serde(default = "default_risk_class")]
    pub default_risk_class: String,
    #[serde(default)]
    pub overrides: HashMap<String, String>,
}

fn default_timeout_secs() -> u64 { 30 }
fn default_auto_reconnect() -> bool { true }
fn default_max_response_bytes() -> usize { 1_048_576 }
fn default_rate_limit_rpm() -> u32 { 60 }
fn default_prefetch_timeout() -> u64 { 300 }
fn default_risk_class() -> String { "exec_capable".into() }
fn star() -> String { "*".into() }
```

---

## Registry

```rust
pub struct CatalogRegistry {
    entries: HashMap<String, CatalogEntry>,   // id -> entry (user entries override built-ins)
}

impl CatalogRegistry {
    pub fn load(user_dir: Option<&Path>) -> Result<Self, AgentOSError> {
        let mut entries = HashMap::new();

        // 1. Embedded built-ins via rust-embed.
        for file in McpCatalogEmbed::iter() {
            let data = McpCatalogEmbed::get(&file)
                .ok_or_else(|| AgentOSError::CatalogParse("missing embed".into()))?;
            let text = std::str::from_utf8(&data.data)?;
            let entry: CatalogEntry = toml::from_str(text)
                .map_err(|e| AgentOSError::CatalogParse(format!("{file}: {e}")))?;
            validate(&entry)?;
            entries.insert(entry.id.clone(), entry);
        }

        // 2. User-local (overrides).
        if let Some(dir) = user_dir {
            if dir.is_dir() {
                for entry in std::fs::read_dir(dir)? {
                    let path = entry?.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                        continue;
                    }
                    let text = std::fs::read_to_string(&path)?;
                    let mut parsed: CatalogEntry = toml::from_str(&text)
                        .map_err(|e| AgentOSError::CatalogParse(format!("{}: {e}", path.display())))?;
                    validate(&parsed)?;
                    // Signature required for non-core community entries from user dir
                    if parsed.trust_tier == TrustTier::Verified && parsed.signature.is_none() {
                        return Err(AgentOSError::CatalogParse(format!(
                            "user catalog entry '{}' declares trust_tier=verified but has no signature",
                            parsed.id
                        )));
                    }
                    entries.insert(parsed.id.clone(), parsed);
                }
            }
        }

        Ok(Self { entries })
    }

    pub fn lookup(&self, id: &str) -> Option<&CatalogEntry> {
        self.entries.get(id)
    }

    pub fn list(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.values()
    }

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
}

fn validate(entry: &CatalogEntry) -> Result<(), AgentOSError> {
    if entry.id.is_empty() || entry.id.contains('/') || entry.id.contains("..") {
        return Err(AgentOSError::CatalogParse(format!("invalid id: {}", entry.id)));
    }
    match (&entry.mcp.transport, &entry.mcp.install) {
        (McpTransportKind::Http, InstallBlock::Prebuilt { .. }) => Ok(()),
        (McpTransportKind::Stdio, InstallBlock::Prebuilt { .. }) => Err(AgentOSError::CatalogParse(
            format!("{}: stdio transport cannot use prebuilt install strategy", entry.id),
        )),
        _ => Ok(()),
    }
}
```

Embed built-ins using `rust-embed`:

```rust
#[derive(rust_embed::RustEmbed)]
#[folder = "../../plugins/mcp-catalog/"]
#[include = "*.toml"]
struct McpCatalogEmbed;
```

---

## Error variants

Add to `crates/agentos-types/src/error.rs`:

```rust
#[error("Catalog entry parse/validation error: {0}")]
CatalogParse(String),

#[error("Catalog entry '{0}' not found")]
CatalogEntryNotFound(String),
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/mcp_catalog.rs` | New module |
| `crates/agentos-kernel/src/lib.rs` | `pub mod mcp_catalog;` |
| `crates/agentos-kernel/Cargo.toml` | Add `rust-embed = "8"`, `toml = "0.8"` (reuse workspace versions) |
| `crates/agentos-types/src/error.rs` | Add `CatalogParse`, `CatalogEntryNotFound` |
| `plugins/mcp-catalog/` | New directory (empty for now; Phase 6 seeds it) |

---

## Dependencies

- **Requires:** Phase 1 (uses `resolve_by_name` for validation — optional at load time, required at install time).
- **Blocks:** Phase 3 (catalog CLI), Phase 4 (install command), Phase 6 (seed entries).

---

## Test plan

1. `CatalogRegistry::load` with empty embed + empty user dir → `Ok(empty)`.
2. Parse a valid `gmail.toml` fixture → all fields populated correctly.
3. Parse invalid id (`"foo/bar"`) → `CatalogParse` error.
4. Parse stdio transport with `Prebuilt` install → validation error.
5. User entry with same id as built-in → user entry wins in `lookup`.
6. User entry claims `trust_tier = "verified"` without signature → `CatalogParse` error.
7. `search("email")` returns Gmail entry from fixtures.
8. `lookup("nonexistent")` returns `None`.

Fixtures under `crates/agentos-kernel/tests/fixtures/mcp-catalog/`.

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel mcp_catalog
cargo clippy -p agentos-kernel -- -D warnings
cargo fmt --all -- --check
```

Expected: all tests pass; registry correctly loads embedded entries once Phase 6 adds them.

---

## Related

- [[MCP Catalog Installer Plan]]
- [[01-runtime-resolver]]
- [[03-catalog-cli-commands]]
- [[04-install-command]]
- [[06-seed-catalog-entries]]
