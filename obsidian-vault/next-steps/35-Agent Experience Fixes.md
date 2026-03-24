---
title: Agent Experience Fixes — Intent Validator, Schema Docs, Memory Read, Escalation Status
tags:
  - kernel
  - tools
  - agent-experience
  - v3
  - next-steps
date: 2026-03-21
status: in-progress
effort: 1d
priority: high
---

# Agent Experience Fixes

> Fix 5 friction points discovered via real agent test run: false-positive intent validation, unclear schema docs, asymmetric memory APIs, and missing escalation introspection.

---

## Current State

An LLM agent test run (2026-03-21) revealed these friction points:
1. Intent validator flags writes to **new** resources as "write-without-read" (false positive)
2. `procedure-create` schema docs in `agent-manual` don't show nested object structure for `steps`
3. `memory-read` only supports semantic scope; agents must discover `memory-search` for episodic
4. Agents cannot check escalation/approval status — no tool exposed (only CLI)

## Goal / Target State

1. Write-without-read only flags **overwrites** of previously-written resources, not first-time writes
2. `agent-manual` tool-detail view shows nested array item schemas (objects with sub-fields)
3. `memory-read` accepts optional `scope` param; `scope=episodic` + `id` reads a specific episode
4. New `escalation-status` tool lets agents query their pending escalations

## Sub-tasks

| # | Task | File | Status |
|---|------|------|--------|
| 01 | [[35-01-Fix Write-Without-Read False Positives]] | `agentos-kernel/src/intent_validator.rs` | planned |
| 02 | [[35-02-Agent Manual Nested Schema Display]] | `agentos-tools/src/agent_manual.rs` | planned |
| 03 | [[35-03-Memory Read Scope Support]] | `agentos-tools/src/memory_read.rs`, `agentos-memory/src/episodic.rs`, `tools/core/memory-read.toml` | planned |
| 04 | [[35-04-Escalation Status Tool]] | `agentos-tools/src/escalation_status.rs`, `tools/core/escalation-status.toml` | planned |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

## Related

- [[31-Agentic Readiness Fixes]]
- [[Memory Context Architecture Plan]]
