---
title: Agent Manual Tool
tags:
  - tools
  - v3
  - next-steps
date: 2026-03-18
status: complete
effort: 2d
priority: high
---

# Agent Manual Tool

> A queryable built-in tool that lets any agent look up OS documentation — tools, permissions, commands, memory, events, errors — on demand, keeping responses under ~500 tokens each.

---

## Current State

Agents discover tools only via `ToolRegistry::tools_for_prompt()` in the system prompt. No runtime docs exist for permissions, kernel commands, memory tiers, events, or error patterns.

## Goal / Target State

A new `agent-manual` tool registered in `ToolRunner` that responds to structured queries like `{"section": "tools"}`, `{"section": "tool-detail", "name": "file-reader"}`, etc. Nine sections total, all JSON responses, all compact.

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[27-01-Define ManualSection Enum and Query Types]] | `crates/agentos-tools/src/agent_manual.rs` | complete |
| 02 | [[27-02-Implement Section Content Generators]] | `crates/agentos-tools/src/agent_manual.rs` | complete |
| 03 | [[27-03-Wire AgentManual into ToolRunner and Registry]] | `crates/agentos-tools/src/runner.rs`, `lib.rs` | complete |
| 04 | [[27-04-Agent Manual Integration Tests]] | `crates/agentos-tools/src/lib.rs` (tests) | complete |
| 05 | [[27-05-Add Tool Manifest and Web UI Link]] | `tools/core/agent-manual.toml`, `crates/agentos-web/` | complete |

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- agent_manual --nocapture
cargo clippy -p agentos-tools -- -D warnings
```

## Related

- [[Agent Manual Plan]] — design rationale and architecture
- [[AgentOS Handbook Index]] — human-facing handbook
