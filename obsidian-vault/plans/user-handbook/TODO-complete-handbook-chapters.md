---
title: "TODO: Complete User Handbook Chapters 07-19"
tags:
  - docs
  - handbook
  - next-steps
date: 2026-03-17
status: planned
effort: 3d
priority: high
---

# Complete User Handbook Chapters 07-19

> Write the remaining 14 handbook chapters (and index) covering Tool System, Security, Vault, Memory, Pipeline, Event, Cost, Audit, Configuration, WASM, Advanced Operations, and Troubleshooting.

## Why This Phase

Chapters 01-06 exist in `obsidian-vault/reference/handbook/`. Chapters 07-19 and the index are entirely missing. V3 features — cost tracking, escalation handling, event subscriptions, resource arbitration, identity management, memory tiers, procedural memory, WASM tools, HAL, and the web UI — have no user-facing documentation. A user or operator approaching AgentOS cannot understand or configure the system without reading source code.

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Handbook chapters | 6 of 19 (01-06) | All 19 chapters + index |
| Tool system docs | None | Chapter 07: trust tiers, signing, SDK macros |
| Security docs | None | Chapter 08: injection scanner, risk classifier, escalation |
| Vault docs | None | Chapter 09: AES-256-GCM, proxy tokens, scopes |
| Memory docs | None | Chapter 10: all tiers, retrieval gate, consolidation |
| Pipeline docs | None | Chapter 11: YAML format, security, pipeline executor |
| Event system docs | None | Chapter 12: EventType registry, subscriptions, filters |
| Cost tracking docs | None | Chapter 13: budget enforcement, model downgrade |
| Audit docs | None | Chapter 14: event types, chain verification, export |
| LLM config docs | None | Chapter 15: provider adapters, model config |
| Config reference | None | Chapter 16: all config keys with defaults |
| WASM dev docs | None | Chapter 17: Wasmtime, tool dev workflow |
| Advanced ops docs | None | Chapter 18: HAL, resource arbitration, snapshots, identity, escalation |
| Troubleshooting | None | Chapter 19: common errors, FAQ |
| Handbook index | None | `AgentOS Handbook Index.md` with all 19 chapter links |

## Detailed Subtasks

For each chapter, read the relevant source files listed in the corresponding plan file before writing. Each chapter must be self-contained.

### Chapter 07 — Tool System
Plan file: `plans/user-handbook/04-tool-system.md`
Source files to read: `crates/agentos-tools/`, `crates/agentos-sdk/src/`, `tools/core/*.toml`
Output: `obsidian-vault/reference/handbook/07-Tool System.md`

### Chapter 08 — Security Model
Plan file: `plans/user-handbook/05-security-and-vault.md`
Source files to read: `crates/agentos-kernel/src/injection_scanner.rs`, `risk_classifier.rs`, `escalation.rs`, `intent_validator.rs`
Output: `obsidian-vault/reference/handbook/08-Security Model.md`

### Chapter 09 — Secrets and Vault
Plan file: `plans/user-handbook/05-security-and-vault.md`
Source files to read: `crates/agentos-vault/src/`, `crates/agentos-kernel/src/commands/secret.rs`
Output: `obsidian-vault/reference/handbook/09-Secrets and Vault.md`

### Chapter 10 — Memory System
Plan file: `plans/user-handbook/06-memory-system.md`
Source files to read: `crates/agentos-memory/src/`, `crates/agentos-kernel/src/retrieval_gate.rs`, `consolidation.rs`, `memory_blocks.rs`, `memory_extraction.rs`
Output: `obsidian-vault/reference/handbook/10-Memory System.md`

### Chapter 11 — Pipeline and Workflows
Plan file: `plans/user-handbook/07-pipeline-event-cost.md`
Source files to read: `crates/agentos-pipeline/src/`, `crates/agentos-kernel/src/commands/pipeline.rs`
Output: `obsidian-vault/reference/handbook/11-Pipeline and Workflows.md`

### Chapter 12 — Event System
Plan file: `plans/user-handbook/07-pipeline-event-cost.md`
Source files to read: `crates/agentos-types/src/event.rs`, `crates/agentos-kernel/src/event_bus.rs`, `event_dispatch.rs`, `trigger_prompt.rs`
Output: `obsidian-vault/reference/handbook/12-Event System.md`

### Chapter 13 — Cost Tracking
Plan file: `plans/user-handbook/07-pipeline-event-cost.md`
Source files to read: `crates/agentos-kernel/src/cost_tracker.rs`
Output: `obsidian-vault/reference/handbook/13-Cost Tracking.md`

### Chapter 14 — Audit Log
Plan file: `plans/user-handbook/08-audit-config-advanced.md`
Source files to read: `crates/agentos-audit/src/log.rs`
Output: `obsidian-vault/reference/handbook/14-Audit Log.md`

### Chapter 15 — LLM Configuration
Plan file: `plans/user-handbook/08-audit-config-advanced.md`
Source files to read: `crates/agentos-llm/src/`
Output: `obsidian-vault/reference/handbook/15-LLM Configuration.md`

### Chapter 16 — Configuration Reference
Plan file: `plans/user-handbook/08-audit-config-advanced.md`
Source files to read: `config/default.toml`, `crates/agentos-kernel/src/config.rs`
Output: `obsidian-vault/reference/handbook/16-Configuration Reference.md`

### Chapter 17 — WASM Tools Development
Plan file: `plans/user-handbook/04-tool-system.md`
Source files to read: `crates/agentos-wasm/src/`, `docs/guide/05-tools-guide.md`
Output: `obsidian-vault/reference/handbook/17-WASM Tools Development.md`

### Chapter 18 — Advanced Operations
Plan file: `plans/user-handbook/08-audit-config-advanced.md`
Source files to read: `crates/agentos-hal/src/`, `crates/agentos-kernel/src/resource_arbiter.rs`, `snapshot.rs`, `identity.rs`, `escalation.rs`
Output: `obsidian-vault/reference/handbook/18-Advanced Operations.md`

### Chapter 19 — Troubleshooting and FAQ
Plan file: `plans/user-handbook/09-troubleshooting-and-index.md`
Source files to read: `crates/agentos-types/src/error.rs`, existing chapters 01-18
Output: `obsidian-vault/reference/handbook/19-Troubleshooting and FAQ.md`

### Handbook Index
Plan file: `plans/user-handbook/09-troubleshooting-and-index.md`
Output: `obsidian-vault/reference/handbook/AgentOS Handbook Index.md`

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/07-Tool System.md` | Create |
| `obsidian-vault/reference/handbook/08-Security Model.md` | Create |
| `obsidian-vault/reference/handbook/09-Secrets and Vault.md` | Create |
| `obsidian-vault/reference/handbook/10-Memory System.md` | Create |
| `obsidian-vault/reference/handbook/11-Pipeline and Workflows.md` | Create |
| `obsidian-vault/reference/handbook/12-Event System.md` | Create |
| `obsidian-vault/reference/handbook/13-Cost Tracking.md` | Create |
| `obsidian-vault/reference/handbook/14-Audit Log.md` | Create |
| `obsidian-vault/reference/handbook/15-LLM Configuration.md` | Create |
| `obsidian-vault/reference/handbook/16-Configuration Reference.md` | Create |
| `obsidian-vault/reference/handbook/17-WASM Tools Development.md` | Create |
| `obsidian-vault/reference/handbook/18-Advanced Operations.md` | Create |
| `obsidian-vault/reference/handbook/19-Troubleshooting and FAQ.md` | Create |
| `obsidian-vault/reference/handbook/AgentOS Handbook Index.md` | Create |

## Dependencies

Chapters 07-18 are independent and can be parallelized. Chapter 19 (Troubleshooting) and the Index should come last as they cross-reference all other chapters.

## Test Plan

- Each chapter must not contain placeholder text or `TODO:` markers
- All CLI commands documented must be verified against `cargo run -p agentos-cli -- --help`
- All config keys must be verified against `config/default.toml`

## Verification

```bash
ls obsidian-vault/reference/handbook/
# Expected: 19 chapter files + AgentOS Handbook Index.md (20 total)
```

## Related

- [[User Handbook Plan]] — master plan with all 9 phases
- [[audit_report]] — GAP-H03
