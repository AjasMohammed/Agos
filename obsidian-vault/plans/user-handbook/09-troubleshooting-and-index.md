---
title: Handbook Troubleshooting and Index
tags:
  - docs
  - v3
  - plan
date: 2026-03-13
status: complete
effort: 3h
priority: high
---

# Handbook Troubleshooting and Index

> Write the Troubleshooting and FAQ chapter and the Handbook Index (table of contents) that ties all 19 chapters together.

---

## Why This Subtask
This is the final subtask. The troubleshooting chapter helps users solve common problems without filing bug reports. The index provides a navigable table of contents with wikilinks to every chapter, serving as the entry point for the entire handbook.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Troubleshooting | None | Comprehensive FAQ covering 20+ common issues |
| Handbook index | Does not exist | Table of contents with chapter summaries and wikilinks |

---

## What to Do

### 1. Write `19-Troubleshooting and FAQ.md`

Read these source files to understand common error paths:
- `crates/agentos-types/src/error.rs` -- `AgentOSError` enum with all error variants
- `crates/agentos-bus/src/message.rs` -- `KernelResponse::Error` format
- `crates/agentos-kernel/src/kernel.rs` -- `Kernel::boot()` for startup failures
- `crates/agentos-cli/src/main.rs` -- CLI error handling
- `obsidian-vault/roadmap/Issues and Fixes.md` -- known bugs and fixes

The chapter must include:

**Section: Common Errors and Solutions**

At minimum, cover these scenarios:

| # | Problem | Solution |
|---|---------|----------|
| 1 | `Config file not found: config/default.toml` | Run from project root or use `--config` flag |
| 2 | `Connection refused` / Cannot reach kernel | Ensure kernel is running with `agentctl start` in another terminal |
| 3 | `Agent '<name>' not found` | Check agent exists with `agentctl agent list`; reconnect if needed |
| 4 | `PermissionDenied` when running a task | Grant required permissions with `agentctl perm grant` |
| 5 | `ToolBlocked` error on tool install | Tool has `trust_tier = "blocked"`; change tier or use a different tool |
| 6 | `ToolSignatureInvalid` on install | Verify signature with `agentctl tool verify`; re-sign with correct key |
| 7 | Vault passphrase forgotten | No recovery; delete vault DB and recreate secrets |
| 8 | Ollama connection error | Ensure Ollama is running (`ollama serve`) and model is pulled |
| 9 | OpenAI/Anthropic/Gemini API errors | Check API key is set in vault (`agentctl secret list`); check endpoint reachability |
| 10 | Task stuck in `Running` state | Check for escalations (`agentctl escalation list`); cancel if needed |
| 11 | `BudgetExceeded` -- task stopped | Check cost report (`agentctl cost show`); increase budget or use cheaper model |
| 12 | Socket path already in use | Previous kernel instance may still be running; kill it or change `[bus].socket_path` |
| 13 | Seccomp sandbox errors on non-Linux | Sandboxing is Linux-only; disable sandbox config on other platforms |
| 14 | WASM tool timeout | Increase `max_cpu_ms` in tool manifest |
| 15 | Memory model download slow | Model cache is at `[memory].model_cache_dir`; ensure sufficient disk space |
| 16 | Audit chain verification fails | Possible data corruption; export chain for forensics with `agentctl audit export` |
| 17 | Pipeline step fails with "skipped" | Check `depends_on` ordering; ensure prerequisite steps completed |
| 18 | Event subscription not firing | Check subscription is enabled (`agentctl event subscriptions list`); check throttle policy |
| 19 | Resource deadlock detected | Use `agentctl resource contention` to inspect; force release with `agentctl resource release` |
| 20 | Escalation auto-expired | Escalations expire after 5 minutes; respond faster or increase timeout |

**Section: Debug Logging**
- `RUST_LOG=agentos=debug cargo run --bin agentos-cli -- start`
- Trace IDs for correlating audit entries
- Using `agentctl audit logs --last N` to investigate

**Section: Checking System Health**
- `agentctl status` -- quick health check
- `agentctl agent list` -- verify agents online
- `agentctl tool list` -- verify tools loaded
- `agentctl audit verify` -- verify audit chain integrity

**Section: Resetting AgentOS**
- How to reset to a clean state (delete `/tmp/agentos/` directories)
- How to preserve secrets while resetting other state

**Section: Platform Notes**
- Linux: full feature set including seccomp sandboxing
- macOS/Windows: sandboxing unavailable (gated by `#[cfg(target_os = "linux")]`)
- Wasmtime: requires compatible host for WASM tool execution

**Section: Getting Help**
- How to file issues
- How to read audit logs for diagnostic information

### 2. Write `AgentOS Handbook Index.md`

This file serves as the table of contents and entry point for the handbook.

```markdown
---
title: AgentOS Handbook Index
tags:
  - docs
  - handbook
date: 2026-03-13
status: planned
---

# AgentOS User Handbook

> The complete guide to installing, configuring, and operating AgentOS.

---

## Chapters

| # | Chapter | Summary |
|---|---------|---------|
| 01 | [[01-Introduction and Philosophy]] | What AgentOS is, core principles, Linux analogy |
| 02 | [[02-Installation and First Run]] | Prerequisites, building, config, first boot |
| 03 | [[03-Architecture Overview]] | System architecture, crate graph, intent flow |
| 04 | [[04-CLI Reference Complete]] | All 18 agentctl command groups with flags and examples |
| 05 | [[05-Agent Management]] | Agent lifecycle, messaging, groups, identity |
| 06 | [[06-Task System]] | Tasks, routing, lifecycle, background, schedules |
| 07 | [[07-Tool System]] | Built-in tools, manifests, trust tiers, signing |
| 08 | [[08-Security Model]] | 7 defense layers, permissions, injection scanner, risk levels |
| 09 | [[09-Secrets and Vault]] | Encrypted vault, secret scopes, rotation, lockdown |
| 10 | [[10-Memory System]] | 4 memory tiers, extraction, consolidation, context budget |
| 11 | [[11-Pipeline and Workflows]] | Multi-step YAML pipelines, failure handling |
| 12 | [[12-Event System]] | Event types, subscriptions, filters, triggered tasks |
| 13 | [[13-Cost Tracking]] | Per-agent costs, budgets, model pricing |
| 14 | [[14-Audit Log]] | Event types, Merkle verification, export, snapshots |
| 15 | [[15-LLM Configuration]] | 5 providers, endpoint resolution, environment variables |
| 16 | [[16-Configuration Reference]] | Every config key with type, default, description |
| 17 | [[17-WASM Tools Development]] | WASM protocol, Rust/Python examples, SDK macros |
| 18 | [[18-Advanced Operations]] | HAL, resource locks, snapshots, escalation, identity |
| 19 | [[19-Troubleshooting and FAQ]] | Common errors, debug logging, platform notes |
```

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/19-Troubleshooting and FAQ.md` | Create new |
| `obsidian-vault/reference/handbook/AgentOS Handbook Index.md` | Create new |

---

## Prerequisites
All previous subtasks ([[01-foundation-chapters]] through [[08-audit-config-advanced]]) must be complete so the index can link to all chapters and the troubleshooting chapter can reference all subsystems.

---

## Test Plan
- Both files exist
- Troubleshooting chapter has >= 20 problem/solution entries
- Index file has 19 rows in the chapter table
- All wikilinks in the index match actual chapter file names (Obsidian file names, not paths)
- `AgentOSError` variants are referenced in the troubleshooting chapter

---

## Verification
```bash
test -f obsidian-vault/reference/handbook/19-Troubleshooting\ and\ FAQ.md
test -f obsidian-vault/reference/handbook/AgentOS\ Handbook\ Index.md

# Index has all 19 chapters
grep -c "\[\[" obsidian-vault/reference/handbook/AgentOS\ Handbook\ Index.md
# Should be >= 19

# Troubleshooting has sufficient entries
grep -c "^\| " obsidian-vault/reference/handbook/19-Troubleshooting\ and\ FAQ.md
# Should be >= 20

# All handbook files present
ls obsidian-vault/reference/handbook/*.md | wc -l
# Should be 20
```
