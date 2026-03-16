---
title: Future Improvements (Post-V1)
tags:
  - roadmap
  - future
  - optimization
  - next-steps
date: 2026-03-13
status: backlog
effort: ongoing
priority: low
---

# Future Improvements (Post-V1)

> Non-critical enhancements, performance optimizations, and architectural improvements identified in the [[16-Full Codebase Review]] that are **not required for the first release**. These can be addressed incrementally after v1.0 ships.

---

## Category 1 — Performance Optimizations

### 1.1 Index Permission Lookups in CapabilityEngine

**Risk Level:** Medium
**Impact:** Improved performance at scale

`validate_intent()` performs a linear scan (`O(n)`) over `token.permissions.entries` for each required permission. This is fine for small permission sets but degrades with many entries.

- [ ] Replace `Vec<PermissionEntry>` with `HashMap<String, PermissionEntry>` in `PermissionSet`
- [ ] Update `check()` to use `O(1)` lookup
- [ ] Benchmark before/after with 100+ permission entries

**File:** `crates/agentos-capability/src/permissions.rs`

---

### 1.2 Bus Client Connection Pooling

**Risk Level:** Low
**Impact:** Reduced overhead for programmatic SDK usage

The `BusClient` creates a new Unix socket connection per CLI invocation. For SDK/API usage, a connection pool is needed.

- [ ] Add connection pool to `BusClient` with configurable max connections
- [ ] Implement connection health checking and automatic reconnection
- [ ] Make pool size configurable via `KernelConfig`

**File:** `crates/agentos-bus/src/client.rs`

---

### 1.3 Configurable Bus Message Size Limit

**Risk Level:** Low
**Impact:** Flexibility for large context windows

The 16MB message size limit is hardcoded. Should be configurable for deployments with very large context windows.

- [ ] Make `MAX_MESSAGE_SIZE` configurable in `BusSettings`
- [ ] Default to 16MB; document the setting

**File:** `crates/agentos-bus/src/transport.rs`

---

### 1.4 SQLite → Specialized Vector Database for Semantic Memory

**Risk Level:** Low (current scale is fine)
**Impact:** Production-scale semantic search

SQLite works for the current scale but will not scale for production workloads with millions of embeddings.

- [ ] Evaluate pgvector, Qdrant, Milvus, or ChromaDB as backends
- [ ] Abstract the storage interface behind a trait
- [ ] Add a config option to select the backend
- [ ] Benchmark query latency at 100K, 1M, 10M embeddings

**File:** `crates/agentos-memory/src/semantic.rs`

---

## Category 2 — Architecture Improvements

### 2.1 Decompose Kernel God Struct

**Risk Level:** Medium
**Impact:** Maintainability, testability

The `Kernel` struct holds ~35 `Arc<...>` fields and `boot()` is 350 lines. Consider grouping into subsystem structs.

- [ ] Create `MemorySubsystem` grouping: `episodic_memory`, `semantic_memory`, `procedural_memory`, `retrieval_gate`, `retrieval_executor`, `memory_extraction`, `consolidation_engine`, `memory_blocks`
- [ ] Create `SecuritySubsystem` grouping: `vault`, `capability_engine`, `identity_manager`, `injection_scanner`, `risk_classifier`
- [ ] Create `SchedulingSubsystem` grouping: `scheduler`, `schedule_manager`, `background_pool`, `cost_tracker`
- [ ] Refactor `Boot()` to use a builder pattern
- [ ] Cross-reference with [[04-Kernel Modularization]]

**File:** `crates/agentos-kernel/src/kernel.rs`

---

### 2.2 Command Registry Pattern

**Risk Level:** Low
**Impact:** Code organization

`handle_command()` is a monolithic `match` with ~50 arms. A command registry pattern would improve extensibility.

- [ ] Create a `CommandHandler` trait with `fn handle(&self, kernel: &Kernel) -> KernelResponse`
- [ ] Register handlers at boot time
- [ ] Dispatch via `HashMap<KernelCommandKind, Box<dyn CommandHandler>>`

**File:** `crates/agentos-kernel/src/run_loop.rs`

---

### 2.3 Remove `async_trait` When Rust Async Traits Stabilize

**Risk Level:** Low
**Impact:** Reduced compilation time, cleaner signatures

The `LLMCore` trait uses `#[async_trait]`. When Rust's native async traits are stable, this can be removed.

- [ ] Track stabilization of `async fn in traits` (RFC 3185)
- [ ] Remove `async_trait` dependency when stable
- [ ] Update all adapter implementations

**File:** `crates/agentos-llm/src/traits.rs`

---

### 2.4 Glob Re-export Cleanup in `agentos-types`

**Risk Level:** Low
**Impact:** Clearer public API

`pub use ids::*;` and `pub use schedule::*;` re-export everything from those modules. Prefer explicit exports.

- [ ] Replace glob re-exports with explicit item lists
- [ ] Run `cargo doc` to verify no regressions

**File:** `crates/agentos-types/src/lib.rs`

---

## Category 3 — Resilience & Observability

### 3.1 LLM Retry Logic with Circuit Breaker

**Risk Level:** Medium
**Impact:** Prevents cascading failures when LLM providers go down

No retry or circuit breaker logic exists in individual LLM adapters. Network errors are propagated raw.

- [ ] Implement exponential backoff + jitter for transient errors (5xx, timeout, connection refused)
- [ ] Add circuit breaker per provider: open after N consecutive failures, half-open after cooldown
- [ ] Make retry count, backoff base, and circuit breaker thresholds configurable
- [ ] Emit events on circuit state transitions

**Files:** `crates/agentos-llm/src/ollama.rs`, `openai.rs`, `anthropic.rs`, `gemini.rs`, `custom.rs`

---

### 3.2 Per-Provider LLM Timeout Configuration

**Risk Level:** Low
**Impact:** Fine-grained timeout control

Currently relies on global reqwest timeout. Different providers have different latency profiles.

- [ ] Add `timeout_secs` to per-provider config sections in `KernelConfig`
- [ ] Apply per-adapter `reqwest::Client` with custom timeout
- [ ] Default to 60s if not specified

**Files:** `crates/agentos-kernel/src/config.rs`, `crates/agentos-llm/src/ollama.rs` (and others)

---

### 3.3 Code Coverage Tracking

**Risk Level:** Low
**Impact:** Visibility into untested code

No code coverage tool is configured. Unknown what percentage of the 44K LoC is covered.

- [ ] Add `cargo-tarpaulin` or `cargo-llvm-cov` to CI
- [ ] Set a minimum coverage threshold (suggest 60% initially)
- [ ] Add coverage badge to `README.md`

**File:** `[NEW] .github/workflows/ci.yml`

---

### 3.4 Audit Log Export

**Risk Level:** Low
**Impact:** Operational visibility

No mechanism to export audit logs for external analysis or compliance.

- [ ] Add `agentctl audit export --format json|csv --since <date>` command
- [ ] Stream to stdout for piping to log aggregators

**Files:** `crates/agentos-audit/src/log.rs`, `crates/agentos-cli/src/commands/audit.rs`

---

## Category 4 — Documentation

### 4.1 Architecture Decision Records (ADRs)

- [ ] Create `docs/adr/` directory
- [ ] Document key decisions: vault design, capability token scheme, bus protocol, seccomp approach

---

### 4.2 CHANGELOG.md

- [ ] Create `CHANGELOG.md` following Keep a Changelog format
- [ ] Backfill from git history for v0.1.0

---

### 4.3 Rustdoc Generation

- [ ] Add `cargo doc --workspace --no-deps` to CI
- [ ] Host generated docs (GitHub Pages or similar)

---

### 4.4 ToolExecutionContext Builder Pattern

**Risk Level:** Low
**Impact:** Cleaner tool test setup

`ToolExecutionContext` has many `Option` fields (`vault`, `hal`, `file_lock_registry`). A builder pattern would simplify test setup.

- [ ] Create `ToolExecutionContextBuilder` with fluent API
- [ ] Update test helpers in `crates/agentos-tools/src/lib.rs`

---

## Category 5 — Platform Support

### 5.1 Sandbox Fallback for Non-Linux Platforms

**Risk Level:** Low
**Impact:** Developer experience on macOS / Windows

The seccomp sandbox is Linux-only. No fallback exists for development on other platforms.

- [ ] Add a no-op `SandboxExecutor` for non-Linux
- [ ] Or use `bwrap`/container-based isolation as a cross-platform alternative
- [ ] Gate with `#[cfg(not(target_os = "linux"))]`

**File:** `crates/agentos-sandbox/src/executor.rs`

---

### 5.2 Token Revocation List for Capability Tokens

**Risk Level:** Low
**Impact:** Defense in depth

Once issued, capability tokens are valid until expiry. There is no way to revoke a specific token.

- [ ] Add a `RevokedTokenSet` to `CapabilityEngine`
- [ ] Check revocation list during `validate_intent()`
- [ ] Add `agentctl perm revoke-token <token_id>` command

**File:** `crates/agentos-capability/src/engine.rs`

---

## Summary

| Category | Items | Effort Estimate |
|---|---|---|
| Performance Optimizations | 4 items | ~3d |
| Architecture Improvements | 4 items | ~5d |
| Resilience & Observability | 4 items | ~4d |
| Documentation | 4 items | ~2d |
| Platform Support | 2 items | ~3d |
| **Total** | **18 items** | **~17d** |

---

## Related

- [[20-V1-Release-Fix-Plan]] — Release-critical fixes (do these first)
- [[16-Full Codebase Review]] — Source audit that produced these findings
- [[V3 Roadmap]] — Broader roadmap including planned features
- [[next-steps/Index|Dashboard]] — Master implementation status
