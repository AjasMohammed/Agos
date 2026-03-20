---
title: Pure Agentic Workflow Compatibility
tags:
  - tools
  - agentic
  - next-steps
  - v3
date: 2026-03-18
status: planned
effort: 5d
priority: critical
---

# Pure Agentic Workflow Compatibility

> Expose 9 hidden tools, add 7 new tools, and extend agent-manual so an LLM agent can operate without any shell hacks or human shortcuts.

---

## Current State

20 tools have TOML manifests. 9 implemented tools are invisible to agents (no manifest). 7 tools needed for a complete agentic loop do not exist. `ToolExecutionContext` lacks two kernel-resource fields needed for agent/task discovery.

## Goal / Target State

Every tool reachable by an agent is listed in `agent-manual`. The agent can: know the time, reason explicitly, discover peers, check delegated tasks, fetch web content as text, and diff files — all within the capability model.

---

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[30-01-Missing Tool Manifests]] | 9 × `tools/core/*.toml` | planned |
| 02 | [[30-02-Think and Datetime Tools]] | `think.rs`, `datetime.rs`, 2 × TOML | planned |
| 03 | [[30-03-Web Fetch Tool]] | `web_fetch.rs`, `web-fetch.toml`, `Cargo.toml` | planned |
| 04 | [[30-04-File Diff Tool]] | `file_diff.rs`, `file-diff.toml`, `Cargo.toml` | planned |
| 05 | [[30-05-Agent Discovery Tool]] | `agent_list.rs`, `agent-list.toml`, `traits.rs`, `agentos-types` | planned |
| 06 | [[30-06-Task Introspection Tools]] | `task_status.rs`, `task_list.rs`, 2 × TOML, `agentos-types` | planned |
| 07 | [[30-07-Agent Manual New Sections]] | `agent_manual.rs` | planned |

---

## Dependency Order

Phases 01–05 are **independent** (can run in parallel).
Phase 06 depends on 05 (shares the `TaskQuery` trait extension to ToolExecutionContext).
Phase 07 depends on 01 (to document newly exposed tools) and 05 (to add agents section).

---

## Verification

After all phases:
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
# Confirm all 36 tools visible in agent-manual
agentctl tool list | wc -l   # expect ≥ 36
```

---

## Related

- [[Agentic Workflow Compatibility Plan]] — design rationale and architecture
- [[Agentic Tool Loop Flow]] — data flow diagram
