---
title: "Phase 4: Developer Experience & Onboarding"
tags:
  - strategy
  - dx
  - onboarding
  - cli
  - phase-4
date: 2026-04-08
status: planned
effort: 2w
priority: critical
---

# Phase 4: Developer Experience & Onboarding

> Reduce the time from first contact to running a working agent on AgentOS to under 5 minutes. Ship a one-line installer, project templates, guided CLI workflows, and a public getting-started guide.

---

## Why This Phase

Research finding: The #1 friction point for complex agent frameworks is the **steep learning curve**. CrewAI wins adoption with 50-line multi-agent setups. PydanticAI wins by leveraging existing FastAPI skills. LangGraph wins with visual debugging in LangGraph Studio.

AgentOS has 27 crates, a kernel boot sequence, capability tokens, and a custom IPC bus. This is powerful but intimidating. Without a guided onboarding path, the security whitepaper (Phase 1) generates interest that can't convert to adoption.

**The goal:** A developer reads the demo, runs `curl | bash`, types `agentos init`, and has a working agent with capability tokens in under 5 minutes. They understand the security model because the template code has inline comments explaining it.

---

## Current → Target State

**Current:** Binary is `agentos`, CLI has 12+ command groups, no `init` scaffolding, no install script, documentation lives in `obsidian-vault/` (internal). Web UI exists but is in-progress.

**Target:** One-line install, `agentos init` templates, public getting-started guide, improved CLI error messages with actionable suggestions.

---

## Detailed Subtasks

### 1. One-Line Install Script

**File:** `scripts/install.sh`

```bash
#!/bin/bash
# Detect OS/arch, download latest release binary, place in PATH
# Supports: Linux x86_64, Linux aarch64, macOS x86_64, macOS aarch64
set -euo pipefail

REPO="your-org/agentos"
VERSION="${AGENTOS_VERSION:-latest}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
# ... download, verify checksum, install to ~/.local/bin/agentos
```

**Verification:** Test on a clean Ubuntu container and macOS (if available).

### 2. `agentos init` Scaffolding Command

Add an `init` subcommand that generates a project from templates:

```bash
# Interactive mode (asks questions)
agentos init

# Template mode
agentos init --template hello-world
agentos init --template secure-agent
agentos init --template mcp-server
agentos init --template multi-agent-team
```

**Templates to ship:**

| Template | Description | Demonstrates |
|----------|-------------|-------------|
| `hello-world` | Minimal agent that responds to a prompt | Basic setup, kernel boot, tool use |
| `secure-agent` | Agent with restricted CapabilityToken | Tokens, permission denial, audit log |
| `mcp-server` | Agent exposed as MCP server | MCP tools, transports, token auth |
| `multi-agent-team` | Coordinator + 2 specialist agents | Teams, sub-agent spawning, context handoff |

**Implementation:**
```rust
// crates/agentos-cli/src/commands/init.rs

pub async fn cmd_init(template: &str, project_name: &str) -> Result<()> {
    // 1. Create project directory
    // 2. Copy template files from embedded assets (rust-embed)
    // 3. Substitute project name in config files
    // 4. Print next-steps instructions
}
```

**Template structure** (each template is a directory under `templates/`):
```
templates/hello-world/
├── agent.toml           # Agent manifest
├── config.toml          # Kernel config (minimal)
├── tools/               # Custom tool manifests
│   └── greet.toml
└── README.md            # Getting started instructions
```

### 3. Guided CLI Error Messages

Audit existing CLI error output and add actionable suggestions:

```
# Before:
Error: KernelNotRunning

# After:
Error: Kernel is not running.

  The AgentOS kernel must be running to execute this command.
  Start it with:

    agentos kernel start

  Or run in foreground for debugging:

    agentos kernel start --foreground
```

**Files:** `crates/agentos-cli/src/main.rs` and error formatting in each command module.

### 4. Public Getting-Started Guide

**File:** `docs/guide/getting-started.md`

**Sections:**
1. **Install** — one-line script or cargo install
2. **Quick Start** — `agentos init --template hello-world` → `agentos kernel start` → `agentos task run`
3. **Understanding the Security Model** — capability tokens explained through the running example
4. **Adding a Custom Tool** — create a tool manifest, register it, use it
5. **Exposing via MCP** — `agentos mcp serve` to make your agent available to external systems
6. **Next Steps** — links to architecture guide, security whitepaper, API reference

### 5. Improve `agentos status` Command

A single command that shows system health at a glance:

```
$ agentos status

  Kernel:     running (pid 12345, uptime 2h 13m)
  Agents:     3 registered, 1 active
  Tasks:      2 completed, 1 running
  Tools:      14 loaded (12 core, 2 community)
  MCP:        serving on stdio + SSE:3001
  Memory:     episodic: 142 entries, semantic: 89, procedural: 12
  Vault:      locked (3 secrets stored)
  Audit:      1,247 events, chain intact
  Budget:     $0.42 / $5.00 daily limit
```

---

## Files Changed

| File | Change |
|------|--------|
| `scripts/install.sh` (new) | One-line installer |
| `crates/agentos-cli/src/commands/init.rs` (new) | Init scaffolding command |
| `crates/agentos-cli/src/commands/mod.rs` | Register init command |
| `crates/agentos-cli/src/main.rs` | Add init subcommand, improve error formatting |
| `templates/` (new dir) | 4 project templates |
| `docs/guide/getting-started.md` (new) | Public getting-started guide |
| `crates/agentos-cli/src/commands/status.rs` | Enhanced status output |

---

## Dependencies

- **Requires:** Phase 1 (demo assets referenced in getting-started guide)
- **Blocks:** Phase 6 (marketplace onboarding depends on `agentos init`)

---

## Test Plan

1. Install script: run on clean Ubuntu 22.04 container → binary available in PATH
2. `agentos init --template hello-world` → project created → `agentos kernel start` → `agentos task run` → agent responds
3. `agentos init --template secure-agent` → deliberately trigger permission denial → correct error shown
4. `agentos status` → all subsystem statuses displayed correctly
5. Time test: fresh install to running agent < 5 minutes
6. `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`

---

## Verification

```bash
# Test install script
bash scripts/install.sh && which agentos

# Test init templates
agentos init --template hello-world --name my-agent
cd my-agent && agentos kernel start --foreground &
sleep 2 && agentos task run --agent my-agent --goal "Say hello"

# Test status
agentos status

# Full workspace check
cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings
```
