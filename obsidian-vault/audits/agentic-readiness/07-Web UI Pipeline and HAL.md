---
title: "Audit #7: Web UI, Pipeline & HAL"
tags:
  - audit
  - web-ui
  - pipeline
  - hal
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 2h
priority: medium
---

# Audit #7: Web UI, Pipeline & HAL

> Evaluating the human oversight interface, workflow orchestration, and hardware abstraction from the perspective of an AI agent operating within this system.

---

## Scope

- `crates/agentos-web/` — Axum server, HTMX templates, chat handlers, auth/CSRF
- `crates/agentos-pipeline/` — YAML-based multi-step workflow engine
- `crates/agentos-hal/` — Hardware device registry, driver abstraction

---

## Verdict: FUNCTIONAL — Web UI needs hardening; Pipeline has injection risk; HAL is well-designed

These subsystems are less critical for pure agentic workflows (I don't use the Web UI directly), but they enable human oversight, workflow automation, and hardware access that are important for production deployment.

---

## Findings

### 1. Web UI — ADEQUATE for oversight

**Architecture:**
- Axum HTTP server with HTMX + MiniJinja templates.
- Auth token (32-byte random) + CSRF protection.
- Rate limiting: 60 req/min burst, 1 req/s steady.
- Security headers: CSP, X-Frame-Options, X-Content-Type-Options.
- Chat interface with SSE streaming.

**What works well for human operators:**
- Dashboard with agent status, task summaries, audit log viewer.
- Chat interface for interactive agent interaction.
- XSS prevention with HTML escaping.
- CSRF tokens on state-changing operations.

**Issues:**

| # | Issue | Severity | Impact |
|---|-------|----------|--------|
| 1 | **Auth token printed to stderr** — container logs may expose it | Medium | Token leakage to log aggregators |
| 2 | **Streaming chat task not tracked** — spawned without JoinHandle, no timeout | High | Background task leak on client disconnect |
| 3 | **Blocking I/O in async handlers** — spawn_blocking for SQLite can starve thread pool | Medium | Latency spikes under concurrent use |
| 4 | **Message size limit 32 KiB** — adequate but not configurable | Low | Can't send larger prompts via chat |
| 5 | **CSRF token sweep every 30 min** — may accumulate tokens if TTL is short | Low | Memory growth |

### 2. Pipeline Engine — GOOD with injection risk

**Architecture:**
- YAML-based pipeline definitions with topological sort.
- Template variable rendering (`{{var}}`).
- On-failure handlers: Fail, Skip, UseDefault.
- Budget check before each step.
- Per-step timeout enforcement.

**What works well for me as an agent:**
- I can define multi-step workflows with dependencies.
- Variables flow between steps (output_var → input).
- Retry with configurable max_attempts.
- Built-in variables: run_id, date, timestamp, input.

**Issues:**

| # | Issue | Severity | Impact |
|---|-------|----------|--------|
| 6 | **Template variable injection** — variables interpolated without escaping into JSON tool inputs and LLM prompts | Critical | If step output contains `","`, it can break JSON; if it contains prompt injection, it gets injected |
| 7 | **No exponential backoff on retries** — immediate retry on failure | Medium | Hammers failing tools |
| 8 | **No expression language** — can't do arithmetic or string manipulation on variables | Low | Limits pipeline expressiveness |

**Template Injection Example:**
```yaml
steps:
  - id: step1
    task: "Write report"
    output_var: report
  - id: step2
    tool: save-file
    input:
      content: "{{report}}"  # If report = 'foo","bar', JSON breaks
```

### 3. Hardware Abstraction Layer — WELL-DESIGNED

**Architecture:**
- `HalDriver` trait for pluggable drivers (system, process, network, storage, GPU, sensor, log_reader).
- `HardwareRegistry` with device lifecycle: Quarantined → Approved/Denied.
- Per-agent access grants on approved devices.

**What works well for me as an agent:**
- Device quarantine by default — new hardware can't be used until approved.
- Per-agent grants — only authorized agents access specific devices.
- Denied devices can't be re-approved (security safeguard).

**Issues:**

| # | Issue | Severity | Impact |
|---|-------|----------|--------|
| 9 | **Process driver permission hardcoded in HAL** — special-case "process" logic breaks abstraction | Medium | New drivers can't define their own permission rules |
| 10 | **No audit trail for device access** — `check_access()` doesn't log usage | Medium | Can't audit who accessed what device |
| 11 | **Status timestamps updated on idempotent calls** — misleading in audit trail | Low | Operational confusion |

---

## Score

| Criterion | Score (1-5) | Notes |
|-----------|------------|-------|
| Web UI | 3.0 | Functional with proper security headers, but streaming leak and auth concerns |
| Pipeline | 3.0 | Good design but critical variable injection risk |
| HAL | 3.5 | Clean abstraction with proper quarantine model |
| **Overall** | **3.2/5** | Functional but needs hardening for production |
