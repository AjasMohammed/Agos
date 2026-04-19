---
title: "Phase 6: Ecosystem & Marketplace"
tags:
  - strategy
  - ecosystem
  - marketplace
  - community
  - phase-6
date: 2026-04-08
status: planned
effort: 2w
priority: medium
---

# Phase 6: Ecosystem & Marketplace

> Build the ecosystem flywheel: make it trivial for external developers to create, publish, and discover AgentOS tools and skills. Leverage MCP as the universal tool interface and the existing marketplace UI for discoverability.

---

## Why This Phase

Research finding: Successful agent frameworks build **ecosystem flywheels** — CrewAI certified 100k+ developers, AutoGPT spawned 400+ forks, MCP enabled hundreds of community-built servers. The pattern: make contribution easy → more tools → more users → more contributors.

AgentOS already has a tool registry (`agentos-registry`), skill system (`agentos-skills`), trust tiers for safety, and a marketplace UI in `agentos-web`. What's missing is the **external developer workflow** — a path from "I have a tool idea" to "it's published and discoverable" that doesn't require understanding 27 crates.

---

## Current → Target State

**Current:** Tool manifests in `tools/core/` and `tools/user/`. Skill manifests as `SKILL.toml`. Marketplace UI exists in web crate. Registry has review table. No external publish workflow, no CLI for package management, no community discovery.

**Target:** `agentos tool publish` and `agentos skill publish` CLI commands, a tool/skill index (local or hosted), MCP-based tool exposure for cross-framework discovery, and contribution guide.

---

## Detailed Subtasks

### 1. Tool SDK Quickstart

Simplify tool creation with the `agentos-sdk` macros:

**File:** `docs/guide/creating-tools.md`

```rust
// Example: a simple tool using the SDK macro
use agentos_sdk::prelude::*;

#[tool(
    name = "word-count",
    description = "Count words in text",
    permissions = ["read"]
)]
async fn word_count(input: WordCountInput) -> Result<ToolOutput> {
    let count = input.text.split_whitespace().count();
    Ok(ToolOutput::json(json!({ "count": count })))
}

#[derive(Deserialize)]
struct WordCountInput {
    text: String,
}
```

Document: how to build, test, sign (Ed25519), and publish a tool.

### 2. Tool Package CLI Commands

```bash
# Create a new tool project
agentos tool new my-tool

# Build and validate tool manifest
agentos tool build

# Sign tool manifest with Ed25519 key
agentos tool sign --key ~/.agentos/author.key

# Verify tool signature
agentos tool verify my-tool.toml

# Publish to local index
agentos tool publish --index ~/.agentos/tool-index/

# Search for tools
agentos tool search "database"

# Install a community tool
agentos tool install my-tool --from ~/.agentos/tool-index/
```

**Most of these already exist** (`tool keygen`, `tool sign`, `tool verify`). New commands needed: `tool new` (scaffold), `tool publish`, `tool search`, `tool install`.

### 3. Skill Package CLI Commands

Mirror the tool workflow for skills:

```bash
# Create a new skill project
agentos skill new my-skill

# Validate SKILL.toml manifest
agentos skill validate

# Publish skill
agentos skill publish --index ~/.agentos/skill-index/

# Search skills
agentos skill search "research"

# Install a community skill
agentos skill install my-skill --from ~/.agentos/skill-index/
```

### 4. Local Package Index

A JSON-based index file that catalogs available tools and skills:

```rust
// crates/agentos-registry/src/index.rs

#[derive(Serialize, Deserialize)]
pub struct PackageIndex {
    pub version: u32,
    pub tools: Vec<PackageEntry>,
    pub skills: Vec<PackageEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub trust_tier: TrustTier,
    pub signature: Option<String>,    // Ed25519 sig
    pub download_url: Option<String>, // For remote index
    pub tags: Vec<String>,
}
```

**Index location:** `~/.agentos/index.json` (local) or fetchable from a URL for hosted index.

### 5. MCP Tool Discovery

Tools published to AgentOS are automatically discoverable via MCP `tools/list`:

```
// When an MCP client calls tools/list, they get all registered tools
// including community-installed ones (with trust tier metadata)
{
  "tools": [
    {
      "name": "word-count",
      "description": "Count words in text",
      "inputSchema": { ... },
      "metadata": {
        "trust_tier": "community",
        "author": "dev@example.com",
        "signed": true
      }
    }
  ]
}
```

### 6. Contribution Guide

**File:** `docs/guide/contributing-tools.md`

Sections:
1. Prerequisites (Rust toolchain, `agentos` binary)
2. Creating a tool (`agentos tool new`)
3. Testing locally (`agentos tool build && cargo test`)
4. Signing (`agentos tool keygen && agentos tool sign`)
5. Publishing (`agentos tool publish`)
6. Trust tiers explained (Community → Verified → Core path)
7. Review process for promotion to Verified/Core

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-cli/src/commands/tool.rs` | Add `new`, `publish`, `search`, `install` subcommands |
| `crates/agentos-cli/src/commands/skill.rs` | Add `new`, `validate`, `publish`, `search`, `install` subcommands |
| `crates/agentos-registry/src/index.rs` (new) | Package index types and operations |
| `crates/agentos-registry/src/lib.rs` | Re-export index module |
| `templates/tool/` (new) | Tool project template |
| `templates/skill/` (new) | Skill project template |
| `docs/guide/creating-tools.md` (new) | Tool SDK guide |
| `docs/guide/contributing-tools.md` (new) | Contribution guide |

---

## Dependencies

- **Requires:** Phase 2 (MCP for tool discovery), Phase 4 (CLI improvements, templates infrastructure)
- **Blocks:** Nothing directly — enables ecosystem growth

---

## Test Plan

1. `agentos tool new my-tool` → project created with valid manifest
2. `agentos tool build` in template project → compiles clean
3. `agentos tool sign && agentos tool verify` → signature valid
4. `agentos tool publish --index /tmp/test-index` → entry added to index
5. `agentos tool search "my-tool" --index /tmp/test-index` → found
6. `agentos tool install my-tool --index /tmp/test-index` → installed and loadable by kernel
7. MCP `tools/list` returns installed community tool with metadata
8. `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`

---

## Verification

```bash
# End-to-end tool lifecycle
agentos tool new test-tool && cd test-tool
agentos tool build && agentos tool sign --key ~/.agentos/author.key
agentos tool publish --index /tmp/idx
agentos tool search "test" --index /tmp/idx
agentos tool install test-tool --index /tmp/idx

# Full workspace check
cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings
```
