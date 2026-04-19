---
title: Phase 3 — Catalog CLI Commands
tags:
  - cli
  - mcp
  - catalog
  - phase-3
date: 2026-04-18
status: planned
effort: 0.5d
priority: medium
---

# Phase 3 — Catalog CLI Commands

> Expose `agentos mcp catalog list | search | info` so users can discover servers before running `mcp install`.

---

## Why this phase

Before a user runs `agentos mcp install gmail`, they need to know that `gmail` is a valid id. The catalog is no use if it's invisible.

Three commands, all read-only, all hitting the kernel over the bus:

- `agentos mcp catalog list` — compact table of every entry.
- `agentos mcp catalog search <query>` — filter by id / name / description.
- `agentos mcp catalog info <id>` — full details for a single entry.

---

## Current → Target

**Current:** `agentos mcp` supports `attach`, `detach`, `status`, `tools`, `call`, `oauth-store`. No catalog commands.

**Target:** three new subcommands wired through the bus.

---

## Detailed subtasks

### 1. Extend `McpCommands` in CLI

**File:** `crates/agentos-cli/src/commands/mcp.rs`

```rust
#[derive(Subcommand)]
pub enum McpCommands {
    // … existing Attach, Detach, etc.

    /// Browse the MCP catalog.
    #[command(subcommand)]
    Catalog(CatalogSubcommand),

    // (Install / Uninstall / Update come in Phase 4.)
}

#[derive(Subcommand)]
pub enum CatalogSubcommand {
    /// List all known catalog entries.
    List {
        /// Show only entries with the given trust tier.
        #[arg(long, value_name = "TIER")]
        trust: Option<String>,
    },

    /// Search catalog entries by id, name, or description.
    Search {
        /// Query string (case-insensitive substring match).
        query: String,
    },

    /// Show full details for a single catalog entry.
    Info {
        /// Catalog id (e.g. "gmail").
        id: String,
    },
}
```

### 2. Kernel commands + dispatch

**File:** `crates/agentos-bus/src/message.rs`

```rust
pub enum KernelCommand {
    // …
    McpCatalogList,
    McpCatalogSearch { query: String },
    McpCatalogInfo { id: String },
    // …
}

pub enum KernelResponse {
    // …
    McpCatalogList { entries: Vec<CatalogSummary> },
    McpCatalogInfo { entry: CatalogEntryJson },
    // …
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSummary {
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub description: String,
    pub trust_tier: String,
    pub transport: String,
    pub runtime: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntryJson(pub serde_json::Value);
```

### 3. Kernel handlers

**File:** `crates/agentos-kernel/src/commands/mcp.rs` (existing file; add handlers)

```rust
impl Kernel {
    pub async fn cmd_mcp_catalog_list(&self) -> KernelResponse {
        let entries: Vec<CatalogSummary> = self.mcp_catalog
            .list()
            .map(|e| CatalogSummary {
                id: e.id.clone(),
                display_name: e.display_name.clone(),
                version: e.version.clone(),
                description: e.description.clone(),
                trust_tier: format!("{:?}", e.trust_tier).to_lowercase(),
                transport: format!("{:?}", e.mcp.transport).to_lowercase(),
                runtime: e.mcp.runtime.clone(),
            })
            .collect();
        KernelResponse::McpCatalogList { entries }
    }

    pub async fn cmd_mcp_catalog_search(&self, query: String) -> KernelResponse {
        let entries = self.mcp_catalog.search(&query)
            .into_iter()
            .map(/* same mapping as above */)
            .collect();
        KernelResponse::McpCatalogList { entries }
    }

    pub async fn cmd_mcp_catalog_info(&self, id: String) -> KernelResponse {
        match self.mcp_catalog.lookup(&id) {
            Some(entry) => match serde_json::to_value(entry) {
                Ok(val) => KernelResponse::McpCatalogInfo { entry: CatalogEntryJson(val) },
                Err(e) => KernelResponse::Error { message: e.to_string() },
            },
            None => KernelResponse::Error {
                message: format!("No catalog entry '{id}'. Try: agentos mcp catalog search <keyword>"),
            },
        }
    }
}
```

Dispatch from `run_loop.rs`:

```rust
KernelCommand::McpCatalogList => self.cmd_mcp_catalog_list().await,
KernelCommand::McpCatalogSearch { query } => self.cmd_mcp_catalog_search(query).await,
KernelCommand::McpCatalogInfo { id } => self.cmd_mcp_catalog_info(id).await,
```

Note: `CatalogEntry` needs `#[derive(Serialize)]` in Phase 2 for `serde_json::to_value` to work. Add to the phase-2 types if not already present.

### 4. CLI handler output

**File:** `crates/agentos-cli/src/commands/mcp.rs`

```rust
pub async fn cmd_catalog_list(bus: &mut BusClient, trust: Option<String>) -> anyhow::Result<()> {
    let resp = bus.send_command(KernelCommand::McpCatalogList).await?;
    match resp {
        KernelResponse::McpCatalogList { mut entries } => {
            if let Some(t) = trust.as_deref() {
                entries.retain(|e| e.trust_tier == t.to_lowercase());
            }
            if entries.is_empty() {
                println!("No catalog entries.");
                return Ok(());
            }
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["ID", "Name", "Version", "Tier", "Transport", "Runtime"]);
            for e in entries {
                table.add_row(vec![
                    e.id, e.display_name, e.version, e.trust_tier,
                    e.transport, e.runtime.unwrap_or_else(|| "-".into()),
                ]);
            }
            println!("{}", table);
        }
        KernelResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("unexpected response"),
    }
    Ok(())
}

pub async fn cmd_catalog_search(bus: &mut BusClient, query: String) -> anyhow::Result<()> {
    // Same rendering as list, but calls McpCatalogSearch.
    // …
}

pub async fn cmd_catalog_info(bus: &mut BusClient, id: String) -> anyhow::Result<()> {
    let resp = bus.send_command(KernelCommand::McpCatalogInfo { id }).await?;
    match resp {
        KernelResponse::McpCatalogInfo { entry } => {
            println!("{}", serde_json::to_string_pretty(&entry.0)?);
        }
        KernelResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("unexpected response"),
    }
    Ok(())
}
```

Route in `main.rs`:

```rust
McpCommands::Catalog(sub) => match sub {
    CatalogSubcommand::List { trust } => commands::mcp::cmd_catalog_list(&mut bus, trust).await?,
    CatalogSubcommand::Search { query } => commands::mcp::cmd_catalog_search(&mut bus, query).await?,
    CatalogSubcommand::Info { id } => commands::mcp::cmd_catalog_info(&mut bus, id).await?,
},
```

### 5. Reuse `comfy-table`

Already used in other commands — no new dep.

---

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-cli/src/commands/mcp.rs` | Add `CatalogSubcommand` + handlers |
| `crates/agentos-cli/src/main.rs` | Route catalog subcommands |
| `crates/agentos-bus/src/message.rs` | Add 3 commands + response variants |
| `crates/agentos-kernel/src/commands/mcp.rs` | Add handlers |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch arms |
| `crates/agentos-kernel/src/mcp_catalog.rs` | Add `Serialize` derive (Phase 2 types) |

---

## Dependencies

- **Requires:** Phase 2 (catalog registry exists).
- **Blocks:** Nothing — this is a read-only convenience layer. Phase 4 can proceed in parallel.

---

## Test plan

1. `cmd_catalog_list` with no entries → "No catalog entries." message.
2. `cmd_catalog_list --trust verified` → only verified-tier rows in table.
3. `cmd_catalog_search "email"` (with gmail seeded) → returns gmail row.
4. `cmd_catalog_info gmail` → prints formatted JSON containing expected keys (`id`, `mcp.install.package`, `mcp.auth.type`).
5. `cmd_catalog_info nonexistent` → exits non-zero with the expected error message.

Integration test: kernel with test registry containing two entries → all 3 CLI commands return expected output.

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-cli mcp::
cargo test -p agentos-kernel catalog

# Manual smoke (after Phase 6 seeds entries)
agentos kernel start
agentos mcp catalog list
agentos mcp catalog search email
agentos mcp catalog info gmail
```

---

## Related

- [[MCP Catalog Installer Plan]]
- [[02-catalog-format-and-registry]]
- [[04-install-command]]
