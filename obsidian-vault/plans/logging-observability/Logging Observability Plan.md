---
title: Logging & Observability Plan
tags:
  - observability
  - logging
  - debugging
  - production
  - plan
date: 2026-03-21
status: planned
effort: 5d
priority: high
---

# Logging & Observability Plan

> Eliminate silent failures and blind spots across the AgentOS runtime by adding structured tracing spans, tool-level logging, and production-ready JSON output with correlation IDs.

---

## Why This Matters

AgentOS currently has a logging *framework* in place (tracing + tracing-subscriber + rolling file appender) but lacks consistent *coverage*. The result is a system where:

- Tasks fail silently — requeue failures in `task_executor.rs` are swallowed with `.ok()` and never logged
- Tool execution is invisible — only 6 tracing calls exist across all tools; file I/O, shell execution, and HTTP calls produce zero log output
- Call chains are untraceable — no `#[instrument]` attributes means there are no spans, so you cannot follow a request from CLI → bus → kernel → tool → result
- Production deployments have no correlation IDs — agent_id and task_id are not injected into the tracing subscriber, so filtering logs by task is impossible

This plan addresses all four gaps in four focused phases.

---

## Current State

| Area | Status | Detail |
|------|--------|--------|
| Logging framework | Implemented | tracing + tracing-subscriber, daily rolling files, RUST_LOG override |
| Auto-instrumentation | None | Zero `#[instrument]` attributes in entire codebase |
| Tools logging | Sparse | 6 tracing calls total; most tools are completely silent |
| Silent failures | 27+ instances | `task_executor.rs` has 27 `.ok()` calls; requeue failures unlogged |
| Correlation IDs | None | No task_id/agent_id in tracing spans; log lines are unattributable |
| JSON output | Available | `tracing-subscriber` has json feature but it is not wired up |
| Log level control | Static | Set at startup; no runtime changes |
| Subsystem restart logging | Missing | `run_loop.rs` restart loop emits no log lines |

---

## Target Architecture

```
CLI Command
    │
    ▼
BusMessage (tagged with correlation_id = task_id)
    │
    ▼
run_loop.rs  ──── #[instrument(skip_all, fields(task_id=%id))]
    │
    ├─► task_executor.rs  ──── span: task_execute{task_id, agent_id}
    │       │
    │       ├─► tool_call.rs  ──── span: tool_call{tool_id, tool_name}
    │       │       │
    │       │       └─► agentos-tools/*  ──── tracing::debug!/info! at key checkpoints
    │       │
    │       └─► scheduler.rs  ──── warn! on requeue failure (was silent .ok())
    │
    └─► event_dispatch.rs  ──── already good; verify span propagation

Log Output (stderr + rolling file):
  - Dev:  human-readable (current format)
  - Prod: JSON lines with correlation fields
    {"timestamp":"...","level":"WARN","target":"...","task_id":"...","agent_id":"...","message":"..."}
```

---

## Phase Overview

| # | Phase | Effort | Depends On | Detail Doc |
|---|-------|--------|------------|------------|
| 1 | Span Instrumentation | 1d | — | [[01-span-instrumentation]] |
| 2 | Tools Crate Logging | 1d | Phase 1 | [[02-tools-logging]] |
| 3 | Silent Failure Elimination | 1d | Phase 1 | [[03-silent-failure-elimination]] |
| 4 | Production Structured Logging | 2d | Phase 1,2,3 | [[04-production-structured-logging]] |

---

## Phase Dependency Graph

```mermaid
graph TD
    P1[Phase 1: Span Instrumentation\n#[instrument] on kernel hot paths\nrun_loop, task_executor, scheduler]
    P2[Phase 2: Tools Logging\ntracing calls in all tools\nfile, shell, http, memory]
    P3[Phase 3: Silent Failure Elimination\n.ok() → warn! + .ok()\nrequeue failures visible]
    P4[Phase 4: Production Structured Logging\nJSON mode, correlation IDs\nlog-level CLI command]

    P1 --> P2
    P1 --> P3
    P2 --> P4
    P3 --> P4

    style P1 fill:#e74c3c,color:#fff
    style P2 fill:#f39c12,color:#fff
    style P3 fill:#f39c12,color:#fff
    style P4 fill:#27ae60,color:#fff
```

---

## Key Design Decisions

1. **Use `#[instrument]` over manual `tracing::span!`** — proc-macro auto-records function args and return values on debug builds; manual spans are error-prone to maintain. Only skip large args (e.g., `ContextWindow`) with `#[instrument(skip(ctx))]`.

2. **Inject task_id/agent_id as span fields, not as log fields** — span fields propagate to all child log events automatically. This means every `tracing::warn!` inside a span tagged with `task_id` will emit `task_id` without the callsite needing to specify it.

3. **Silent `.ok()` calls become `if let Err(e) = ... { tracing::warn!(...) }` not hard errors** — requeue failures are genuinely non-fatal; the right signal is a warn, not a task abort. This preserves existing semantics while making failures visible.

4. **JSON mode controlled by config, not compile flag** — `config/default.toml` gains a `log_format = "text" | "json"` field. Dev defaults to `text`; production deployments set `json`. No recompile needed.

5. **No new crate dependencies** — all four phases use crates already in `Cargo.toml` (`tracing`, `tracing-subscriber` with json feature, `tracing-appender`). Zero new deps.

6. **Tools log at `debug` level by default** — tool execution details are high-volume and only needed when debugging. Using `debug` means they are silent in production unless `RUST_LOG=debug` or a targeted filter like `RUST_LOG=agentos_tools=debug` is set.

---

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| `#[instrument]` on hot async paths adds span overhead | Low | Spans are cheap (~50ns); only add to non-tight-loop functions |
| JSON log format breaks existing log parsers | Medium | Make it opt-in via config; default remains text |
| Logging requeue failures at warn may be noisy under load | Low | Add a debounce counter or rate-limit the warn to once per N failures |
| Tool debug logs are too verbose in prod | Low | Default level is `info`; `debug` requires explicit RUST_LOG opt-in |

---

## Related

- [[32-Logging Observability]] — implementation checklist
- [[Logging Observability Data Flow]] — data flow diagram
- [[01-span-instrumentation]] — Phase 1 detail
- [[02-tools-logging]] — Phase 2 detail
- [[03-silent-failure-elimination]] — Phase 3 detail
- [[04-production-structured-logging]] — Phase 4 detail
