---
title: "Phase 7: Cross-Cutting Review Passes"
tags:
  - review
  - security
  - concurrency
  - quality
  - phase-7
date: 2026-03-13
status: complete
effort: 3h
priority: high
---

# Phase 7: Cross-Cutting Review Passes

> Six targeted sweeps across the entire codebase, each focusing on a single concern that spans multiple crates.

---

## Why This Phase

Some bugs are invisible when reviewing code crate-by-crate — they only emerge when you look for the same pattern across the whole system. A single `unwrap()` in a production path, one `format!()` in a SQL query, or one lock held across an `.await` can be catastrophic. These passes catch what per-crate review misses.

---

## Step 7.1 — `unwrap()` / `expect()` Audit (3 sub-steps)

**Method:** Search all `.rs` files for `unwrap()` and `expect()`. For each occurrence: is it test-only, provably infallible, or a bug?

**Step 7.1a — Foundation + Infrastructure crates** (~9 crates)
- Search: `crates/agentos-types/`, `agentos-audit/`, `agentos-vault/`, `agentos-llm/`, `agentos-hal/`, `agentos-memory/`, `agentos-pipeline/`, `agentos-capability/`, `agentos-bus/`

**Step 7.1b — Kernel crate** (~49 files)
- Search: `crates/agentos-kernel/`
- Extra attention to `task_executor.rs` (largest file, highest density risk)

**Step 7.1c — CLI + Tools + Web** (~5 crates)
- Search: `crates/agentos-cli/`, `agentos-tools/`, `agentos-web/`, `agentos-wasm/`, `agentos-sdk*/`

**Checklist per occurrence:**
- [ ] Is it inside `#[cfg(test)]`? (acceptable)
- [ ] Is it provably infallible? (e.g., `"literal".parse::<Regex>().unwrap()`) — document why
- [ ] Is it a bug? → Record as finding (severity depends on crash impact)
- [ ] Does `expect()` message leak internals?

---

## Step 7.2 — Concurrency & Async Safety

**Files to review:** All files using `Arc<RwLock<>>`, channels, spawned tasks:
- `crates/agentos-kernel/src/kernel.rs`
- `crates/agentos-kernel/src/run_loop.rs`
- `crates/agentos-kernel/src/task_executor.rs`
- `crates/agentos-kernel/src/resource_arbiter.rs`
- `crates/agentos-kernel/src/event_bus.rs`
- `crates/agentos-kernel/src/agent_message_bus.rs`

**Checklist:**
- [ ] No `RwLock` held across `.await` points (deadlock risk with tokio)
- [ ] No `Mutex` held across `.await` points
- [ ] `CancellationToken` checked in all long-running loops
- [ ] `spawn_blocking` used for blocking I/O (SQLite, filesystem)
- [ ] No unbounded channel usage (bounded channels or explicit backpressure)
- [ ] All spawned tasks are tracked (joinable or with cancellation)

---

## Step 7.3 — SQL Injection Audit

**Files:** All 5 files using rusqlite:
- `crates/agentos-audit/src/log.rs`
- `crates/agentos-memory/src/episodic.rs`
- `crates/agentos-memory/src/semantic.rs`
- `crates/agentos-pipeline/src/store.rs`
- `crates/agentos-vault/src/vault.rs`

**Checklist:**
- [ ] Every SQL query uses `?1`, `?2` parameterized syntax
- [ ] No `format!()` or string concatenation in SQL statements
- [ ] Table/column names are not user-controlled
- [ ] PRAGMA statements are hardcoded (not user-influenced)
- [ ] No dynamic SQL generation from untrusted input

---

## Step 7.4 — Secret Hygiene

**Files:**
- `crates/agentos-vault/src/vault.rs`, `master_key.rs`, `crypto.rs`
- `crates/agentos-capability/src/engine.rs`
- `crates/agentos-kernel/src/identity.rs`
- `crates/agentos-tools/src/signing.rs`
- `crates/agentos-kernel/src/commands/secret.rs`

**Checklist:**
- [ ] All key material uses `ZeroizingString` or `Zeroize` derive
- [ ] No `Debug` impl that prints key bytes
- [ ] API keys (LLM providers) not logged
- [ ] Error messages do not include secret values
- [ ] Temporary buffers containing secrets are zeroed after use

---

## Step 7.5 — Test Coverage Gap Analysis

**Method:** Compare tested modules against untested modules.

**Currently tested (have test files):**
- `agentos-cli`: 5 integration tests (800 lines)
- `agentos-tools`: 2 test files (292 lines)
- `agentos-sdk`: 1 test file (69 lines)
- Various crates: inline `#[cfg(test)]` modules

**Likely untested (no test files — check for inline tests):**
- `agentos-bus` — no tests
- `agentos-capability` — inline tests only
- `agentos-vault` — inline tests only
- `agentos-kernel` — all 49 files (relies on CLI integration tests)
- `agentos-hal` — no tests
- `agentos-memory` — no tests
- `agentos-pipeline` — no tests
- `agentos-sandbox` — no tests
- `agentos-wasm` — no tests
- `agentos-web` — no tests

**Deliverable:** Top 10 most impactful missing tests, prioritized by risk.

---

## Step 7.6 — API Surface Consistency

**Files:** All 17 `lib.rs` files (~300 lines total)

**Checklist:**
- [ ] Only intended types are `pub use` re-exported
- [ ] No accidental exposure of internal types
- [ ] Module visibility is intentional (`pub mod` vs `mod`)
- [ ] Consistent naming conventions across crates
- [ ] Re-exports match crate root documentation

---

## Findings

| File | Line(s) | Severity | Category | Description | Fix Applied |
|------|---------|----------|----------|-------------|-------------|
| `crates/agentos-audit/src/log.rs` | `verify_chain()` | WARNING | Silent failure | If the DB query for a predecessor hash fails (e.g., due to row deletion), `.ok()` silently mapped the error to `None`, allowing a corrupted chain to pass verification | Yes — now propagates `AgentOSError::VaultError` |
| `crates/agentos-kernel/src/kernel.rs` | boot | WARNING | File permissions | Vault parent directory created with default umask; on shared systems the `/tmp` path is world-listable | Yes — added `set_permissions(0o700)` on parent dir at startup |
| `config/default.toml` | 13 | INFO | Config documentation | `/tmp` vault path documented without security warning | Yes — added warning comment with production path guidance |

## Remaining Issues

None — all findings remediated.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-audit/src/log.rs` | `verify_chain()` predecessor hash error propagation |
| `crates/agentos-kernel/src/kernel.rs` | Vault parent dir `0o700` permissions at boot |
| `config/default.toml` | Security warning comment on `/tmp` vault path |

## Dependencies

Phases 1-6 complete (per-crate review provides context for cross-cutting analysis).

## Verification

N/A — produces findings document.

---

## Related

- [[Full Codebase Review Plan]]
- [[08-security-deep-dives]]
- [[10-synthesis-and-report]]
