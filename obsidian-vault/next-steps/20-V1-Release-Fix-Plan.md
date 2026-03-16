---
title: V1 Release Fix Plan
tags:
  - release
  - v1
  - security
  - kernel
  - next-steps
date: 2026-03-13
status: planned
effort: ~10d
priority: critical
---

# V1 Release Fix Plan

> Comprehensive multistep plan for all fixes required before the first public release of AgentOS. Derived from [[16-Full Codebase Review]] (2026-03-13 audit of all 17 crates, 44K LoC).

---

## Overview

This plan covers **release-blocking issues only** — items that would cause crashes, security vulnerabilities, data loss, or production failures if shipped as-is. Items that are enhancements or optimizations without safety impact are tracked separately in [[21-Future-Improvements]].

> [!important]
> Every phase must pass `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` before merging. No phase depends on an unfinished prior phase unless marked with `depends-on`.

---

## Phase 1 — Critical Safety Fixes (~2d)

> Fixes that prevent panics, data loss, or silent security failures in core paths.

### 1.1 Replace `std::sync::Mutex` with `tokio::sync::Mutex` in Vault

**Risk:** R2 — `std::sync::Mutex<Connection>` blocks Tokio worker threads during SQLite operations, causing runtime stalls under load.

**Affected files:**
- `crates/agentos-vault/src/vault.rs`

**Steps:**
- [ ] Replace `std::sync::Mutex<Connection>` with `tokio::sync::Mutex<Connection>`
- [ ] Update all `self.conn.lock().unwrap()` calls to `self.conn.lock().await`
- [ ] Make `set()`, `get()`, `list()`, `revoke()`, `rotate()`, `check_scope()`, `is_initialized()` async
- [ ] Update all callers in `agentos-kernel` to `.await` the vault calls
- [ ] Update `proxy_tokens` to `tokio::sync::Mutex<HashMap<...>>` as well
- [ ] Run `cargo test --workspace` to verify no deadlocks introduced

> [!warning]
> This is a breaking API change across the vault boundary. All kernel command handlers that call vault methods will need updates.

---

### 1.2 Fix Audit Error Suppression in Vault

**Risk:** R5 — `let _ = self.audit.append(...)` in `set()`, `get()`, `revoke()`, `rotate()`, `lockdown()`, `issue_proxy_token()` silently discards audit write failures. Security events can be lost without any trace.

**Affected files:**
- `crates/agentos-vault/src/vault.rs`

**Steps:**
- [ ] Create a `vault_audit_log()` helper (similar to `Kernel::audit_log()`) that calls `tracing::error!` on failure
- [ ] Replace all `let _ = self.audit.append(...)` with the new helper
- [ ] Verify audit error paths are logged via `tracing` in tests

---

### 1.3 Fix Poisoned Mutex Handling in Vault

**Risk:** R9 — Vault uses `lock().unwrap()` which panics on poisoned mutexes. The capability engine already uses the safe pattern.

**Affected files:**
- `crates/agentos-vault/src/vault.rs`

**Steps:**
- [ ] Replace all `self.conn.lock().unwrap()` with `self.conn.lock().unwrap_or_else(|e| e.into_inner())`
- [ ] Replace all `self.proxy_tokens.lock().unwrap()` with the same pattern
- [ ] Add `tracing::warn!` on poisoned lock recovery (match the pattern in `capability/engine.rs`)

---

### 1.4 Fix `unwrap()` on Corrupt Date Parsing in Vault

**Risk:** R9 — `vault.rs:251` calls `.unwrap()` on `chrono::DateTime::parse_from_rfc3339()` in `list()`. Corrupt data panics the kernel.

**Affected files:**
- `crates/agentos-vault/src/vault.rs`

**Steps:**
- [ ] Replace `.unwrap()` on datetime parsing with `.unwrap_or_else(|_| chrono::Utc::now())` or return an error
- [ ] Add a test case for corrupt date handling

---

### 1.5 Bound Event Channels

**Risk:** R3 — All 4 event channels use `tokio::sync::mpsc::unbounded_channel()`. Under heavy event load, these grow without limit and can exhaust memory.

**Affected files:**
- `crates/agentos-kernel/src/kernel.rs` (channel creation in `boot()`)

**Steps:**
- [ ] Replace `unbounded_channel()` with `channel(capacity)` for:
  - [ ] `event_sender` / `event_receiver` — capacity: `10_000`
  - [ ] `tool_lifecycle_sender` / `tool_lifecycle_receiver` — capacity: `1_000`
  - [ ] `comm_notif_sender` / `comm_notif_receiver` — capacity: `1_000`
  - [ ] `schedule_notif_sender` / `schedule_notif_receiver` — capacity: `1_000`
- [ ] Update senders to handle `SendError` (log + drop if full)
- [ ] Add capacity constants to `config.rs` (configurable via TOML)

---

## Phase 2 — Security Hardening (~2d)

> Closes security gaps that could be exploited in a deployed instance.

### 2.1 Narrow HMAC Signing Key Scope

**Risk:** R10 — The HMAC signing key is stored with `SecretScope::Global`, meaning any agent could potentially access it through the vault.

**Affected files:**
- `crates/agentos-capability/src/engine.rs`

**Steps:**
- [ ] Change `SecretScope::Global` to `SecretScope::Agent(kernel_agent_id)` or introduce a new `SecretScope::Kernel` variant
- [ ] If adding `SecretScope::Kernel`, update `agentos-types/src/secret.rs` and the vault's `check_scope()` method
- [ ] Verify no agent can access `__internal_hmac_signing_key` via proxy token
- [ ] Add a test that agent proxy token issuance fails for kernel-scoped secrets

---

### 2.2 Event HMAC Signing and Audit Trail Completion

**Risk:** Events emitted from `AgentMessageBus` and `ScheduleManager` bypass HMAC signing and audit logging.

**Affected files:**
- `crates/agentos-kernel/src/agent_message_bus.rs`
- `crates/agentos-kernel/src/schedule_manager.rs`
- `crates/agentos-kernel/src/run_loop.rs`

**Steps:**
- [ ] Ensure all event emissions from these subsystems go through the kernel's `emit_event()` helper
- [ ] Verify HMAC signatures are present on all events in integration tests
- [ ] Cross-reference with [[02-event-hmac-audit-fix]]

> [!note]
> This is tracked as Issue #9 in the [[next-steps/Index|Dashboard]]. Completing it here closes that gap.

---

### 2.3 Per-Agent and Per-IP Rate Limiting

**Risk:** Current rate limiting is per-connection only (50 cmd/s). A malicious agent could open multiple connections.

**Affected files:**
- `crates/agentos-kernel/src/rate_limit.rs`
- `crates/agentos-kernel/src/run_loop.rs`

**Steps:**
- [ ] Add a global `RateLimitRegistry` keyed by agent ID (or connection peer address for unauthenticated connections)
- [ ] Make per-agent limits configurable in `KernelConfig`
- [ ] Emit an audit event on rate limit violations
- [ ] Add tests for multi-connection rate limit enforcement

---

## Phase 3 — Build & CI Infrastructure (~2d)

> Establishes the quality gates needed for a responsible release.

### 3.1 Set Up CI Pipeline

**Risk:** R4 — No CI pipeline means regressions go undetected.

**Affected files:**
- `[NEW] .github/workflows/ci.yml`

**Steps:**
- [ ] Create GitHub Actions workflow with:
  - [ ] `cargo fmt --all -- --check`
  - [ ] `cargo clippy --workspace -- -D warnings`
  - [ ] `cargo test --workspace`
  - [ ] `cargo build --release --workspace`
- [ ] Add caching for `target/` and `~/.cargo/`
- [ ] Trigger on `push` to `main` and all PRs
- [ ] Add badge to `README.md`

---

### 3.2 Fix Remaining Clippy Errors

**Risk:** 4 clippy errors block CI gate adoption.

**Affected files:**
- Per [[01-clippy-ci-gate-fixes]], see: `commands/escalation.rs`, `event_bus.rs`, `event_dispatch.rs`, `memory_extraction.rs`

**Steps:**
- [ ] Run `cargo clippy --workspace -- -D warnings`
- [ ] Fix all errors (dead code, unused imports, lint warnings)
- [ ] Cross-reference with [[01-clippy-ci-gate-fixes]]

---

### 3.3 Docker Deployment Artifacts

**Risk:** R7 — No containerized deployment blocks production use.

**Affected files:**
- `[NEW] Dockerfile`
- `[NEW] docker-compose.yml`
- `[NEW] .dockerignore`
- `[NEW] config/docker.toml`

**Steps:**
- [ ] Create multi-stage `Dockerfile`:
  - Stage 1: `rust:1.75-slim` builder
  - Stage 2: `debian:bookworm-slim` runtime
  - Non-root user (`agentos`)
  - `HEALTHCHECK` instruction using `/health` endpoint
  - `ENTRYPOINT` with graceful SIGTERM handling
- [ ] Create `.dockerignore` (`.git`, `target/`, `node_modules/`, `.env*`, tests, IDE configs)
- [ ] Create `config/docker.toml` with persistent `/data` mount paths
- [ ] Create `docker-compose.yml` for single-node dev deployment (Ollama + AgentOS)
- [ ] Verify: `docker build -t agentos:v1 .` && `docker run --health-cmd` passes
- [ ] Cross-reference with [[04-docker-deployment-artifacts]]

---

## Phase 4 — Testing & Stability (~2d)

> Establishes baseline test coverage for release confidence.

### 4.1 Integration Test Harness

**Risk:** R6 — Empty `tests/` directory; no end-to-end validation.

**Affected files:**
- `[NEW] tests/e2e/mod.rs`
- `[NEW] tests/e2e/kernel_boot.rs`
- `[NEW] tests/e2e/agent_lifecycle.rs`
- `[NEW] tests/e2e/tool_execution.rs`
- `[NEW] tests/e2e/permission_flow.rs`

**Steps:**
- [ ] Create test harness that boots a `Kernel` in-process with a temp data directory
- [ ] Add E2E test: kernel boot → status check → shutdown
- [ ] Add E2E test: agent connect → task run → result → disconnect
- [ ] Add E2E test: tool execution → permission check → path traversal block
- [ ] Add E2E test: secret set → proxy token → resolve → revoke
- [ ] Remove `#[ignore]` from existing integration tests and ensure they pass
- [ ] Cross-reference with [[03-integration-test-harness]]

---

### 4.2 Audit Log Rotation

**Risk:** R8 — SQLite audit log grows unbounded; will fill disk in production.

**Affected files:**
- `crates/agentos-audit/src/log.rs`
- `crates/agentos-kernel/src/config.rs`

**Steps:**
- [ ] Add `max_audit_entries` and `rotation_policy` to `AuditSettings` in config
- [ ] Implement rotation: when entry count exceeds limit, archive oldest N entries to a compressed file
- [ ] Add `audits/` archive directory to `data_dir`
- [ ] Add a test for rotation trigger and archive integrity

---

## Phase 5 — Kernel Stabilization (~2d)

> Improves kernel resilience for sustained production operation.

### 5.1 Improve JoinSet Panic Identification

**Risk:** R12 — When a subsystem task panics, the kernel cannot identify which task crashed and restarts all 9 tasks.

**Affected files:**
- `crates/agentos-kernel/src/run_loop.rs`

**Steps:**
- [ ] Use `tokio::task::Builder::new().name("task_kind").spawn()` inside `JoinSet` to name tasks
- [ ] Alternatively, wrap each spawned future in a catch_panic layer that returns `(TaskKind, Result)` instead of just `TaskKind`
- [ ] Log the exact crashed task and restart only that one
- [ ] Add a test for single-task panic → single-task restart behavior

---

### 5.2 Episodic Memory Write on Task Completion

**Risk:** Episodic auto-write on task completion is marked "Not started" in the dashboard.

**Affected files:**
- `crates/agentos-kernel/src/task_executor.rs`
- Cross-reference: [[05-Episodic Memory Completion]]

**Steps:**
- [ ] On task completion (success or failure), write an episodic entry summarizing the task
- [ ] Include: task prompt, agent, tools used, result summary, duration, error (if any)
- [ ] Scope the entry to the agent that executed the task
- [ ] Add a test verifying episodic entries are created on task completion

---

## Verification Checklist

```bash
# Phase-by-phase verification
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --release --workspace

# Docker (Phase 3.3)
docker build -t agentos:v1 .
docker run --rm agentos:v1 --help

# Integration (Phase 4.1)
cargo test --test integration_test
```

---

## Summary

| Phase | Effort | Items | Priority |
|---|---|---|---|
| Phase 1 — Critical Safety | ~2d | 5 tasks | **Blocker** |
| Phase 2 — Security Hardening | ~2d | 3 tasks | **Critical** |
| Phase 3 — Build & CI | ~2d | 3 tasks | **Critical** |
| Phase 4 — Testing & Stability | ~2d | 2 tasks | **High** |
| Phase 5 — Kernel Stabilization | ~2d | 2 tasks | **High** |
| **Total** | **~10d** | **15 tasks** | |

---

## Related

- [[16-Full Codebase Review]] — Source audit that produced these findings
- [[21-Future-Improvements]] — Non-critical items deferred to post-v1
- [[18-Bug Fixes and Deployment Readiness]] — Overlapping items (clippy, Docker, test harness)
- [[next-steps/Index|Dashboard]] — Master implementation status
- [[12-Production Readiness Audit]] — Prior production readiness analysis
