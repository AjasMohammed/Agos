---
title: Agent Marketplace
tags:
  - marketplace
  - tools
  - adoption
  - plan
  - v3
date: 2026-03-25
status: planned
effort: 8d
priority: high
---

# Phase 4 — Agent Marketplace

> Add a tool and agent registry so developers can discover, install, and publish community tools with a single command (`agentctl tool add <name>`), backed by the existing Ed25519 trust tier system.

---

## Why This Phase

The ecosystem research identifies agent/tool marketplaces as a primary adoption driver:

> "A structured Trust Tier System (Core, Verified, Community) would allow users to safely browse and install third-party agents and tools, with untrusted 'Community' code strictly restricted by seccomp-BPF sandboxing."

AgentOS already has **everything needed for the security model** — Ed25519 signing, trust tiers, seccomp-BPF sandboxing for Community tools. What's missing is the registry index and install UX. This is primarily a distribution problem, not a security problem.

This phase also directly solves the **user bootstrap problem**: without a host OS shell, the web UI (Subtask 4.6) becomes the only way to install tools. That makes 4.6 a first-class deliverable, not an afterthought.

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Tool discovery | Manual: copy TOML to `tools/user/`, restart | `agentctl tool add <name>` from registry |
| Local file install | `agentctl tool install <path>` (exists) | Unchanged — keep for developer workflows |
| Tool publishing | Manual: sign with `agentctl tool sign`, distribute file | `agentctl tool publish <path>` → registry |
| Community tools | None exist | Curated starter set (WASM/Rust only in v1) |
| Trust enforcement | Already complete for installed tools | Unchanged — Community tools still run under seccomp sandbox |
| Browsing | None | `agentctl tool search <query>`, web UI marketplace page |
| Version management | None | `agentctl tool update`, `agentctl tool list --outdated` |
| Standalone OS install | Impossible without host OS shell | Web UI marketplace is primary install path |

---

## Architecture

```
Developer                       Registry Server (hosted or self-hosted)
    │                                    │
    │  agentctl tool publish tool.toml   │
    │─────────────────────────────────►  │
    │                                    │  Verify Ed25519 signature
    │                                    │  Store manifest + binary/wasm
    │                                    │  Index for search
    │
User (CLI)
    │  agentctl tool search "github"
    │─────────────────────────────────►  Registry API (GET /v1/tools?q=github)
    │◄────────────────────────────────── List of matching tools
    │
    │  agentctl tool add github-pr-reviewer
    │─────────────────────────────────►  Registry API (GET /v1/tools/github-pr-reviewer)
    │◄────────────────────────────────── Tool manifest (TOML) + implementation
    │  Verify Ed25519 signature locally
    │  Write to tools/user/<name>.toml
    │  Send KernelCommand::ToolLoad to kernel (hot-reload, no restart)

User (Web UI — primary path when no host OS shell)
    │  Browser → /marketplace
    │  Search, click Install
    │  Web handler verifies + hot-loads via KernelCommand::ToolLoad
```

---

## Recommended Implementation Order

```
4.0 (prereq) → 4.1 → 4.2 → 4.3 → 4.4 → 4.5 → 4.6 → 4.7 → 4.8
```

- **4.0** must come first — CLI and web UI both depend on `KernelCommand::ToolLoad`
- **4.6** (web UI) is a first-class deliverable for the standalone OS scenario, not optional
- **4.7** (starter tools) is optional at launch — prioritize WASM/Rust over Python for portability
- **4.8** (update/outdated) can ship as a follow-up

---

## Detailed Subtasks

### Subtask 4.0 — Prerequisite: `KernelCommand::ToolLoad`

> This is a blocking prerequisite. Subtasks 4.4 and 4.6 both call `KernelCommand::ToolLoad` which does not exist yet. The existing `KernelCommand::InstallTool { manifest_path }` only reads from disk — it does not hot-reload a freshly downloaded tool. A dedicated `ToolLoad` command is needed.

**Files:**
- `crates/agentos-bus/src/message.rs` — add `ToolLoad` variant
- `crates/agentos-kernel/src/commands/tool.rs` — add `cmd_tool_load` handler
- `crates/agentos-kernel/src/lib.rs` — dispatch arm

```rust
// crates/agentos-bus/src/message.rs
pub enum KernelCommand {
    // ...existing...
    /// Hot-reload a tool from an already-written manifest path (no restart required).
    /// Used by `agentctl tool add` and the web UI marketplace after writing tools/user/<name>.toml.
    ToolLoad { manifest_path: String },
}

// crates/agentos-kernel/src/commands/tool.rs
pub(crate) async fn cmd_tool_load(&self, manifest_path: String) -> KernelResponse {
    // Same as cmd_install_tool but emits a ToolLoaded audit event
    // and returns the assigned ToolID on success.
    let path = std::path::Path::new(&manifest_path);
    let content = match std::fs::read_to_string(path) { ... };
    let manifest = match toml::from_str::<ToolManifest>(&content) { ... };
    match self.tool_registry.write().await.register(manifest) {
        Ok(id) => KernelResponse::Success {
            data: Some(serde_json::json!({ "tool_id": id.to_string() }))
        },
        Err(e) => KernelResponse::Error { message: e.to_string() },
    }
}
```

---

### Subtask 4.1 — Registry API specification

The marketplace registry is a simple HTTP REST API. Define the contract first.

**Endpoints (v1 scope — tools only; agent profiles deferred to a later phase):**

```
GET  /v1/tools                        List all tools (paginated, sorted by downloads)
GET  /v1/tools?q=<query>              Search tools by name/description/tags
GET  /v1/tools/<name>                 Get tool manifest + metadata
GET  /v1/tools/<name>/versions        List all versions of a tool
GET  /v1/tools/<name>/<version>       Get specific version manifest
GET  /v1/tools/<name>/<version>/dl    Download tool implementation (wasm/binary)
POST /v1/tools                        Publish new tool (requires author key)
POST /v1/tools/<name>/versions        Publish new version
```

> **Agent profiles (`/v1/agents`) are deferred.** The API spec originally included agent registry endpoints but no subtask implemented them. They are out of scope for this phase to keep the registry focused.

**Tool manifest schema** (extends existing TOML format):

```toml
[manifest]
name = "github-pr-reviewer"
version = "1.2.0"
description = "Review GitHub pull requests, post comments, and approve/request changes"
author = "alice"
author_pubkey = "ed25519:<hex public key>"
signature = "ed25519:<hex signature over canonical JSON>"
trust_tier = "community"
tags = ["github", "code-review", "collaboration"]
license = "MIT"
homepage = "https://github.com/alice/agentos-github-tool"
downloads = 1247
created_at = "2026-01-15T00:00:00Z"
updated_at = "2026-03-10T00:00:00Z"

[manifest.permissions]
allowed = ["network.outbound:x", "user.notify:w"]

[implementation]
# v1: wasm | rust (compiled binary)
# python | shell deferred — requires a separate script execution phase
kind = "wasm"
entrypoint = "tool.wasm"

[input_schema]
# JSON Schema for tool parameters
```

---

### Subtask 4.2 — Registry server implementation

**Directory:** `crates/agentos-registry/` (new crate)

This is a minimal Axum-based HTTP server that can be self-hosted or run as a public service.

```rust
// Cargo.toml
[package]
name = "agentos-registry"
version = "0.1.0"

[dependencies]
axum = { workspace = true }
tokio = { workspace = true }
serde = { workspace = true }
rusqlite = { workspace = true }
ed25519-dalek = { workspace = true }  # reuse from agentos-tools
```

**Data model** (SQLite):

```sql
CREATE TABLE tools (
    name          TEXT NOT NULL,
    version       TEXT NOT NULL,
    manifest      TEXT NOT NULL,  -- full TOML as JSON
    author        TEXT NOT NULL,
    author_pubkey TEXT NOT NULL,
    signature     TEXT NOT NULL,
    tags          TEXT NOT NULL,  -- JSON array
    downloads     INTEGER DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (name, version)
);

CREATE VIRTUAL TABLE tools_fts USING fts5(
    name, description, tags, author,
    content=tools
);
```

**Signature verification on publish:**

```rust
async fn publish_tool(
    Json(req): Json<PublishRequest>,
) -> Result<Json<PublishResponse>, RegistryError> {
    // 1. Parse manifest
    let manifest = parse_manifest(&req.manifest_toml)?;
    // 2. Verify Ed25519 signature (reuse agentos-tools signing logic)
    verify_manifest_signature(&manifest)?;
    // 3. Store in SQLite
    db.insert_tool(&manifest).await?;
    Ok(Json(PublishResponse { name: manifest.name, version: manifest.version }))
}
```

**Config:** Add `[registry]` section to `config/default.toml`:

```toml
[registry]
url = "https://registry.agentos.dev"   # default public registry
# url = "http://localhost:8090"         # for local/self-hosted
```

Read via `get_registry_url()` in CLI, falling back to `AGENTOS_REGISTRY` env var.

---

### Subtask 4.3 — CLI: `agentctl tool search`

**File:** `crates/agentos-cli/src/commands/tool.rs`

Add `search` subcommand:

```rust
#[derive(Subcommand)]
pub enum ToolCommand {
    // ...existing...
    Search {
        query: String,
        #[arg(long, default_value = "20")]
        limit: u32,
    },
}

async fn cmd_tool_search(query: &str, limit: u32) -> Result<()> {
    let registry_url = get_registry_url()?;  // from config or env AGENTOS_REGISTRY
    let client = reqwest::Client::new();
    let tools: Vec<ToolListing> = client
        .get(format!("{}/v1/tools?q={}&limit={}", registry_url, query, limit))
        .send().await?
        .json().await?;

    // Pretty-print table: name, version, author, downloads, description
    print_tool_table(&tools);
    Ok(())
}
```

Output:
```
$ agentctl tool search github

  Name                      Version  Author   Downloads  Description
  github-pr-reviewer        1.2.0    alice    1,247      Review PRs, post comments
  github-issue-tracker      0.9.1    bob        342      Create and update GitHub issues
  github-file-browser       1.0.0    carol      891      Browse repos, read files
```

---

### Subtask 4.4 — CLI: `agentctl tool add` (registry install)

> **Important:** The existing `agentctl tool install <path>` command (local file install) is **preserved unchanged** — it is the developer workflow. The new `agentctl tool add <name>` command is for registry-based installs. These are separate subcommands to avoid a breaking change.

**File:** `crates/agentos-cli/src/commands/tool.rs`

```rust
// New subcommand — registry install
Add {
    name: String,
    #[arg(long)]
    version: Option<String>,
    #[arg(long)]
    yes: bool,  // skip confirmation prompt
},

async fn cmd_tool_add(
    client: &mut BusClient,
    name: &str,
    version: Option<&str>,
    yes: bool,
) -> Result<()> {
    // 1. Fetch manifest from registry
    let manifest = fetch_manifest_from_registry(name, version).await?;

    // 2. Show trust tier + permissions — user must confirm
    println!("Tool:        {} v{}", manifest.name, manifest.version);
    println!("Trust tier:  {:?}", manifest.trust_tier);
    println!("Permissions: {:?}", manifest.permissions.allowed);
    println!("Author:      {} ({})", manifest.author, manifest.author_pubkey);

    if !yes {
        confirm("Install this tool? [y/N]")?;
    }

    // 3. Verify Ed25519 signature locally (never trust registry blindly)
    verify_manifest_signature(&manifest)?;

    // 4. Download implementation to tools/user/<name>/
    download_tool_impl(&manifest).await?;

    // 5. Write manifest TOML to tools/user/<name>.toml
    let manifest_path = format!("tools/user/{}.toml", name);
    write_manifest_toml(&manifest, &manifest_path)?;

    // 6. Hot-reload via KernelCommand::ToolLoad (added in Subtask 4.0)
    let response = client
        .send_command(KernelCommand::ToolLoad { manifest_path })
        .await?;

    match response {
        KernelResponse::Success { .. } => println!("✓ {} v{} installed", manifest.name, manifest.version),
        KernelResponse::Error { message } => bail!("Install failed: {}", message),
        _ => bail!("Unexpected response"),
    }
    Ok(())
}
```

---

### Subtask 4.5 — CLI: `agentctl tool publish`

**File:** `crates/agentos-cli/src/commands/tool.rs`

```rust
Publish {
    manifest_path: PathBuf,
    #[arg(long)]
    key: Option<PathBuf>,  // Ed25519 private key path (default: ~/.agentos/identity.key)
},

async fn cmd_tool_publish(manifest_path: &Path, key_path: Option<&Path>) -> Result<()> {
    // 1. Load and validate manifest TOML
    let manifest = load_manifest(manifest_path)?;

    // 2. Sign manifest with author keypair (reuse existing tool sign logic)
    let signed = sign_manifest(&manifest, key_path)?;

    // 3. POST to registry
    let registry_url = get_registry_url()?;
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/tools", registry_url))
        .json(&PublishRequest { manifest_toml: signed.to_toml() })
        .send().await?;

    if resp.status().is_success() {
        println!("✓ Published {} v{}", manifest.name, manifest.version);
        println!("  View at: {}/tools/{}", registry_url, manifest.name);
    } else {
        bail!("Publish failed: {}", resp.text().await?);
    }
    Ok(())
}
```

---

### Subtask 4.6 — Web UI: marketplace browse page

> **This subtask is a first-class deliverable, not optional.** When AgentOS runs standalone (no host OS shell), the web UI is the **only** way for a user to install tools. The CLI workflow requires creating a `.toml` on the host filesystem — impossible in a fully self-contained OS. The web UI solves the bootstrap problem.

**File:** `crates/agentos-web/src/templates/marketplace/index.html` (new)

- Search box → HTMX GET to `/api/marketplace/tools?q=`
- Tool cards: name, version, author, trust tier badge, download count, description
- "Install" button → POST to `/api/marketplace/tools/<name>/install`
- Filter by: trust tier, tag, implementation kind (wasm/rust)
- Installed tools show a checkmark; already-installed "Install" button becomes "Reinstall"

**File:** `crates/agentos-web/src/handlers/marketplace.rs` (new)

```rust
// POST /api/marketplace/tools/:name/install
async fn install_tool(
    Path(name): Path<String>,
    State(kernel): State<Arc<Kernel>>,
) -> impl IntoResponse {
    // 1. Fetch manifest from configured registry
    let manifest = fetch_manifest_from_registry(&name, None).await?;
    // 2. Verify Ed25519 signature locally
    verify_manifest_signature(&manifest)?;
    // 3. Download implementation
    download_tool_impl(&manifest).await?;
    // 4. Write manifest TOML
    let manifest_path = format!("tools/user/{}.toml", name);
    write_manifest_toml(&manifest, &manifest_path)?;
    // 5. Hot-reload via kernel
    kernel.cmd_tool_load(manifest_path).await
}
```

**File:** `crates/agentos-web/src/router.rs` — add marketplace routes.

---

### Subtask 4.7 — Starter community tool set

> **v1 scope: WASM and Rust compiled tools only.** Python and shell tools require a separate script execution engine (sandboxed interpreter, runtime isolation) that is out of scope for this phase. Shipping Python tools first would create a poor first impression if the execution path is not production-ready.

Publish 5 reference community tools to the registry:

| Tool Name | Description | Kind | Permissions |
|-----------|-------------|------|-------------|
| `github-file-reader` | Read files from public GitHub repos | wasm | `network.outbound:x` |
| `slack-notifier` | Send Slack messages via webhook | wasm | `network.outbound:x`, `user.notify:w` |
| `csv-analyzer` | Parse and summarize CSV files | wasm | `fs.user_data:r` |
| `weather-checker` | Get current weather for a location | wasm | `network.outbound:x` |
| `json-formatter` | Pretty-print and validate JSON | wasm | (none) |

All signed with a `verified` author key and uploaded to the registry as the reference set.

---

### Subtask 4.8 — CLI: `agentctl tool update` and `--outdated`

**File:** `crates/agentos-cli/src/commands/tool.rs`

```rust
// List installed tools with available updates
List {
    #[arg(long)]
    outdated: bool,  // show only tools with newer registry versions
},

// Update one or all tools
Update {
    name: Option<String>,  // if None, update all installed tools
    #[arg(long)]
    yes: bool,
},
```

`list --outdated` fetches the latest version from the registry for each installed tool and compares against the installed version. `update` calls `cmd_tool_add` for each outdated tool.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-bus/src/message.rs` | Add `ToolLoad` variant to `KernelCommand` |
| `crates/agentos-kernel/src/commands/tool.rs` | Add `cmd_tool_load` handler |
| `crates/agentos-kernel/src/lib.rs` | Add dispatch arm for `ToolLoad` |
| `crates/agentos-registry/` | New crate — registry HTTP server |
| `crates/agentos-cli/src/commands/tool.rs` | Add `search`, `add`, `publish`, `update` subcommands; keep existing `install` |
| `crates/agentos-web/src/templates/marketplace/index.html` | New — marketplace browse page |
| `crates/agentos-web/src/handlers/marketplace.rs` | New — marketplace proxy handler |
| `crates/agentos-web/src/router.rs` | Add marketplace routes |
| `tools/community/` | New directory — starter community WASM tools |
| `config/default.toml` | Add `[registry]` section with default URL |

---

## Dependencies

- **4.0 (`KernelCommand::ToolLoad`) must be completed first** — both CLI (4.4) and web UI (4.6) depend on it
- Ed25519 signing infrastructure already complete (`agentos-tools`)
- Trust tier enforcement already complete (kernel)
- WASM execution already available (`agentos-wasm`)
- Script tool execution (Python/shell) is **not** a dependency — deferred to a future phase

---

## Known Gaps (deferred)

| Gap | Notes |
|-----|-------|
| Python/shell tool execution | Requires sandboxed interpreter, runtime isolation — separate phase |
| Agent profile registry (`/v1/agents`) | Out of scope for this phase; API spec reserved for future use |
| Tool yanking / security advisories | Registry should support yanking a compromised version; deferred |
| Offline install (air-gapped) | `agentctl tool install <path>` already covers this for now |

---

## Test Plan

1. **Publish and install roundtrip** — publish a test tool to local registry, `agentctl tool add` it, verify it appears in `agentctl tool list`
2. **Signature verification on install** — tamper with manifest after signing, attempt install, assert rejection with `ToolSignatureInvalid`
3. **Community tool sandboxed** — install a community WASM tool, execute it, verify it runs under seccomp policy
4. **Search returns results** — publish 3 tools with different tags, search by tag, assert correct subset returned
5. **Version selection** — publish v1.0.0 and v1.1.0, `agentctl tool add foo --version 1.0.0`, assert v1.0.0 installed
6. **Hot-reload (no restart)** — add a tool while kernel is running, assert it's immediately callable without restart
7. **Web UI install** — install a tool via the marketplace web page, verify kernel registers it
8. **Existing `install` not broken** — `agentctl tool install ./my-tool.toml` still works after CLI changes

---

## Verification

```bash
# Start local registry
cargo run -p agentos-registry -- --port 8090

# Set registry URL
export AGENTOS_REGISTRY=http://localhost:8090

# Publish a test tool (developer workflow)
agentctl tool publish tools/community/csv-analyzer.toml

# Search
agentctl tool search csv

# Install from registry (user workflow — kernel must be running)
agentctl tool add csv-analyzer

# Verify installed + hot-loaded (no restart)
agentctl tool list | grep csv-analyzer

# Check for updates
agentctl tool list --outdated

# Update all
agentctl tool update --yes

# Local file install still works (developer workflow)
agentctl tool install ./my-local-tool.toml
```

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[03-python-sdk]] — Python tools use the same manifest format for publishing
