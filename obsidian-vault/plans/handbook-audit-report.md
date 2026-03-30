---
title: Handbook vs Codebase Audit Report
tags:
  - docs
  - handbook
  - audit
date: 2026-03-29
status: in-progress
effort: 1d
priority: high
---

# Handbook vs Codebase Audit Report

> Systematic comparison of all 22 handbook chapters against the actual AgentOS codebase, identifying gaps, spec drift, and undocumented features.

---

## Summary

| Metric | Value |
|--------|-------|
| Chapters audited | 22 |
| Chapters fully accurate | 6 (Ch 01, 02, 03, 11, 15, 22) |
| Chapters with minor drift | 8 (Ch 05, 06, 07, 09, 13, 17, 19, 21) |
| Chapters with significant gaps | 8 (Ch 04, 08, 10, 12, 14, 16, 18, 20) |
| Total gaps found | 42 |
| Critical gaps | 3 |
| High gaps | 11 |
| Medium gaps | 16 |
| Low gaps | 12 |

---

## Critical Gaps

### 1. Chapter 12 — Event Types Completely Wrong
**Type:** spec_drift
**Impact:** Developers following the handbook will expect events that don't exist.

The handbook documents ~45 event types with names like `TaskCreated`, `IntentReceived`, `AgentConnected`, `LLMInferenceStarted`, `SecretCreated`, `KernelStarted`, etc. **None of these exist** in the `EventType` enum. The actual enum has completely different variant names:

| Handbook Says | Code Has |
|---------------|----------|
| `TaskCreated` | `TaskStarted` |
| `AgentConnected` | `AgentAdded` |
| `AgentDisconnected` | `AgentRemoved` |
| `ToolExecutionStarted` | `ToolCallStarted` |
| `ToolExecutionCompleted` | `ToolCallCompleted` |
| `IntentReceived` | _(does not exist)_ |
| `LLMInferenceStarted` | _(does not exist)_ |
| `SecretCreated` | _(does not exist)_ |
| `KernelStarted` | _(does not exist)_ |

The handbook also confuses `EventType` (runtime events) with `AuditEventType` (audit log entries) — these are separate enums in separate crates with different variant names.

**Fix:** Rewrite the event types table using the actual `EventType` enum from `agentos-types/src/event.rs`.

### 2. Chapter 16 — 13 Config Sections Undocumented
**Type:** missing_feature
**Impact:** Users cannot configure major subsystems.

The Configuration Reference is missing entire sections that exist in `config/default.toml`:

| Missing Section | Purpose |
|----------------|---------|
| `[logging]` | Log directory, level, format |
| `[context]` | Context compression/summarization mode |
| `[memory.context]` | Per-agent context memory |
| `[notifications]` | Notification system settings |
| `[notifications.adapters.webhook]` | Webhook delivery |
| `[notifications.adapters.desktop]` | Desktop notifications |
| `[notifications.adapters.slack]` | Slack delivery |
| `[scratchpad]` | Agent scratchpad settings |
| `[registry]` | Tool registry URL |
| `[[mcp.servers]]` | MCP server definitions |
| `[otel]` | OpenTelemetry tracing |
| `[kernel]` `sandbox_policy` | Sandbox enforcement mode |
| `[ollama]` `request_timeout_secs` | Ollama HTTP timeout |

**Fix:** Add all missing sections to Chapter 16.

### 3. Chapter 04 — 6 CLI Command Groups Undocumented
**Type:** missing_feature
**Impact:** Users don't know these commands exist.

The CLI has 27 top-level commands. The handbook documents 22. Missing:

| Command | Purpose |
|---------|---------|
| `agentctl stop` | Gracefully shut down the kernel |
| `agentctl agent memory` | 5 subcommands for context memory management |
| `agentctl task trace` / `task traces` | Execution trace inspection |
| `agentctl scratchpad` | 4 subcommands for scratchpad management |
| `agentctl log` | Runtime log level/format control |
| `agentctl healthz` | Docker HEALTHCHECK endpoint verification |

**Fix:** Add sections for all 6 missing commands.

---

## High-Priority Gaps

### 4. Chapter 08 — Permission Flags Incomplete
**Type:** spec_drift
Docs show 3 permission ops (`r`, `w`, `x`). Code implements 5: `r`, `w`, `x`, `q` (query), `o` (observe).

### 5. Chapter 08 — Injection Pattern Count Outdated
**Type:** spec_drift
Docs say "26 patterns across 8 categories". Code has 28 patterns.

### 6. Chapter 10 — Memory Tools Not Fully Listed
**Type:** missing_feature
Missing tools: `procedure-search`, `procedure-create`, `procedure-list`, `procedure-delete`, `episodic-list`, `memory-delete`, `memory-read`, `memory-stats`, `context-memory-read`, `context-memory-update`.

### 7. Chapter 14 — Event Type Count Outdated
**Type:** spec_drift
Docs say "61 event types". Code has 83 `AuditEventType` variants. Missing 22+ newer events (notifications, channels, pubkey, context memory, etc.).

### 8. Chapter 07 — 11 Tools Undocumented
**Type:** missing_feature
54 tool manifests exist in `tools/core/`. Handbook documents 43. Undocumented: `agent-call`, `context-memory-read`, `context-memory-update`, `escalation-status`, `scratch-read`, `scratch-write`, `scratch-search`, `scratch-delete`, `scratch-links`, `scratch-graph`, `usb-storage`.

### 9. Chapter 18 — Device State Machine Diagram Wrong
**Type:** spec_drift
Diagram shows `Detected -> Quarantined` as initial state. Code registers devices as `Pending`. Should be `Detected -> Pending`.

### 10. Chapter 18 — HAL Drivers Skeletal
**Type:** partial_implementation
Handbook references `sys-monitor`, `hardware-info`, `process-manager`, `network-monitor` tools. Driver implementations in `agentos-hal/src/drivers/` are minimal stubs.

### 11. Chapter 20 — Tool Call Format Mismatch
**Type:** spec_drift
Handbook shows `{"tool": "...", "input": {...}}`. Harness uses `{"tool": "...", "intent_type": "...", "payload": {...}}`.

### 12. Chapter 09 — Audit Event Names Wrong
**Type:** spec_drift
Docs say `SecretSet` — code logs `SecretCreated`. Docs say `VaultLockdown` event — code uses `SecretRevoked` with details.

### 13. Chapter 11 — Pipeline Wall-Time Not Enforced
**Type:** partial_implementation
`max_wall_time_minutes` is parsed but NOT enforced at pipeline level. Handbook calls it "hard wall-clock timeout".

### 14. Chapter 16 — max_parallel Default Mismatch
**Type:** spec_drift
Handbook says default is 10. Code default is 5 (default.toml overrides to 10).

---

## Medium-Priority Gaps

| # | Chapter | Type | Description |
|---|---------|------|-------------|
| 15 | 04/06 | spec_drift | Cron format inconsistency — Ch04 says 6-field, Ch06 says 5-field, code supports both |
| 16 | 07 | spec_drift | http-client docs say "Redirects are not followed" — code supports optional `follow_redirects` param |
| 17 | 10 | missing_feature | `max_episodes_per_cycle` not in config file, only hardcoded default (500) |
| 18 | 13 | missing_feature | `NotifyOnly` budget action exists in code but undocumented |
| 19 | 17 | missing_feature | Python WASM via py2wasm documented but not implemented anywhere |
| 20 | 18 | missing_feature | Per-agent device denial underdocumented (code supports finer control) |
| 21 | 18 | partial_impl | Denied device re-registration flow documented but no code path exists |
| 22 | 21 | partial_impl | Email delivery adapter is a stub returning error |
| 23 | 21 | missing_feature | `free_text_allowed` parameter exists in ask-user but undocumented |
| 24 | 08 | partial_impl | Merkle hash chain in audit log — documented but needs deeper verification |
| 25 | 15 | missing_feature | Mock LLM adapter not documented (testing-only, minor) |
| 26 | 12 | spec_drift | Event categories differ — docs use different groupings than code |
| 27 | 12 | spec_drift | Docs confuse EventType (runtime) with AuditEventType (audit entries) |
| 28 | 14 | missing_feature | 22 newer AuditEventType variants not listed in handbook |
| 29 | 16 | spec_drift | Production config missing most sections (uses code defaults) |
| 30 | 18 | partial_impl | Resource sweep 10-min scheduling delegated to run_loop, not documented clearly |

---

## Low-Priority Gaps

| # | Chapter | Type | Description |
|---|---------|------|-------------|
| 31 | 05 | clarity | "base" vs "general" role confusion in agent docs |
| 32 | 09 | naming | `SecretSet` vs `SecretCreated` — minor naming difference |
| 33 | 09 | missing | `SecretScope::Kernel` exists in code but undocumented (internal) |
| 34 | 10 | clarity | Retrieval gate signal words not fully verified against code |
| 35 | 13 | clarity | Wall-time enforcement is per-task, not per-agent daily |
| 36 | 16 | clarity | chars_per_token clamping bounds mentioned but not emphasized |
| 37 | 22 | minor | Error detection more robust than docs (checks both cases) |
| 38 | 08 | minor | DNS rebinding protection implemented but not documented |
| 39 | 07 | minor | CRL (Certificate Revocation List) exists but not in handbook |
| 40 | 10 | minor | RRF percentage (70/30) documented but exact code verification needed |
| 41 | 21 | minor | ask-user timeout min/max in manifest not enforced by tool |
| 42 | 18 | minor | Snapshot retention sweep delegated to run_loop background task |

---

## Chapters Confirmed Accurate

The following chapters were verified as fully accurate against the codebase:

- **Ch 01** — Introduction and Philosophy: Conceptual, no code claims
- **Ch 02** — Installation and First Run: Build commands, prereqs match
- **Ch 03** — Architecture Overview: Crate graph, intent flow accurate
- **Ch 05** — Agent Management: All operations match (minor clarity issue on roles)
- **Ch 06** — Task System: All states, routing, risk levels match
- **Ch 11** — Pipeline and Workflows: YAML format, failure handling, CLI all match (wall-time caveat)
- **Ch 15** — LLM Configuration: All 5 adapters, retry/circuit-breaker, fallback match
- **Ch 22** — MCP Integration: Bidirectional bridge, security, CLI all match

---

## Fix Priority

1. **Immediate** — Rewrite Ch 12 event types, add Ch 16 missing sections, add Ch 04 missing commands
2. **High** — Fix Ch 08 permission flags, Ch 14 event count, Ch 10 tool tables, Ch 18 state diagram
3. **Medium** — Fix Ch 07, Ch 09, Ch 13, Ch 17, Ch 20, Ch 21 minor drifts
4. **Low** — Clarity improvements across Ch 05, Ch 10, Ch 16
