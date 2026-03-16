# AgentOS — Comprehensive Codebase Review

> **Date:** 2026-03-13  
> **Scope:** Full workspace analysis across all 17 crates  
> **Lines of Code:** ~44,237 Rust LoC | 124 source files | 270 unit/integration tests

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Project Overview & Architecture](#2-project-overview--architecture)
3. [Crate-by-Crate Analysis](#3-crate-by-crate-analysis)
4. [Security Review](#4-security-review)
5. [Error Handling](#5-error-handling)
6. [Performance & Concurrency](#6-performance--concurrency)
7. [Testing](#7-testing)
8. [Configuration & Deployment](#8-configuration--deployment)
9. [Code Quality & Maintainability](#9-code-quality--maintainability)
10. [Documentation](#10-documentation)
11. [Risk Register & Technical Debt](#11-risk-register--technical-debt)
12. [Recommendations](#12-recommendations)

---

## 1. Executive Summary

AgentOS is an ambitious, well-engineered Rust workspace that implements an LLM-native operating environment. The project demonstrates **strong security fundamentals** (AES-256-GCM vault, HMAC-signed capability tokens, scope enforcement, seccomp sandboxing), **solid async architecture** (Tokio-based supervised run loop with automatic restart budgets), and **comprehensive tooling** (25+ built-in tools, WASM executor, multi-provider LLM adapters).

### Strengths
- **Security-first design** — Zero-exposure vault proxy, capability tokens, secret scope enforcement, emergency lockdown
- **Supervised kernel** — 9 core tasks with crash restart + budget limits; graceful degradation
- **Comprehensive CLI** — 17 command groups with inline parse tests
- **Rich type system** — 20+ typed error variants via `thiserror`; strong enum modeling throughout
- **Multi-provider LLM support** — Ollama, OpenAI, Anthropic, Gemini, Custom + Mock
- **Memory architecture** — Three-tier (episodic/semantic/procedural) with hybrid vector + FTS search

### Areas for Improvement
- **Kernel monolith** — `Kernel` struct holds ~35 `Arc` fields; `boot()` is a 350-line function
- **Test coverage gaps** — 270 tests is good, but `tests/` dir is empty; no E2E framework
- **Missing CI pipeline** — `.github/` exists but no workflow YAML visible
- **Inconsistent `unwrap()` usage** — Some `Mutex::lock().unwrap()` calls in critical paths
- **Audit log fire-and-forget** — Multiple `let _ = self.audit.append(...)` suppress errors silently

---

## 2. Project Overview & Architecture

### High-Level Architecture

```
CLI (agentctl)
    │  Unix Domain Socket / TLS
    ▼
Inference Kernel  ← Central orchestrator
    ├── Task Scheduler       ├── Context Manager/Compiler
    ├── Agent Registry       ├── Tool Registry + Runner
    ├── Capability Engine    ├── Secrets Vault
    ├── Message Bus          ├── Event Bus + Dispatcher
    ├── Memory (3-tier)      ├── Pipeline Engine
    ├── HAL                  ├── Snapshot Manager
    ├── Risk Classifier      ├── Injection Scanner
    ├── Cost Tracker         ├── Resource Arbiter
    └── Health Monitor       └── Escalation Manager
    ├── LLM Adapters         ├── WASM Executor
    └── Sandbox (seccomp)    └── Audit Log
```

### Dependency Flow

```
agentos-types  ← Foundation (no internal deps)
    ↓
agentos-audit  ← Depends on types
agentos-vault  ← Depends on types + audit
agentos-capability ← Depends on types + vault
    ↓
agentos-bus, agentos-llm, agentos-memory, agentos-tools, agentos-sandbox, agentos-wasm, agentos-hal
    ↓
agentos-pipeline, agentos-sdk, agentos-sdk-macros
    ↓
agentos-kernel  ← Depends on EVERYTHING
    ↓
agentos-cli    ← Depends on kernel + bus
```

> [!IMPORTANT]
> The dependency graph is clean and acyclic. `agentos-types` is the leaf with zero internal deps, and `agentos-kernel` is the root that aggregates all subsystems.

### Lines of Code by Crate

| Crate | LoC | Role |
|---|---:|---|
| `agentos-kernel` | 20,915 | Central orchestrator — by far the largest |
| `agentos-cli` | 4,290 | CLI interface (17 command groups) |
| `agentos-tools` | 3,967 | 25+ built-in tool implementations |
| `agentos-memory` | 2,418 | Three-tier memory (episodic/semantic/procedural) |
| `agentos-types` | 2,270 | Shared types, IDs, errors, event types |
| `agentos-pipeline` | 1,810 | Multi-agent pipeline engine |
| `agentos-llm` | 1,616 | LLM adapters (5 providers + mock) |
| `agentos-hal` | 1,357 | Hardware Abstraction Layer |
| `agentos-sandbox` | 931 | Seccomp-BPF sandbox execution |
| `agentos-audit` | 905 | Append-only SQLite audit log |
| `agentos-vault` | 882 | Encrypted secrets vault |
| `agentos-bus` | 818 | Unix socket IPC + TLS |
| `agentos-web` | 799 | Web UI (Axum) |
| `agentos-capability` | 660 | HMAC-signed capability tokens |
| `agentos-wasm` | 284 | Wasmtime-based WASM tool executor |
| `agentos-sdk-macros` | 214 | Proc macros for SDK |
| `agentos-sdk` | 101 | SDK public API |
| **Total** | **44,237** | |

---

## 3. Crate-by-Crate Analysis

### `agentos-types` — Shared Foundation

**Purpose:** Defines all shared types, IDs, error types, event types, and domain models.

**Strengths:**
- Clean module structure with well-organized re-exports in `lib.rs`
- UUID-backed ID types (`AgentID`, `TaskID`, `TraceID`, etc.) providing type safety
- Rich enum modeling for `IntentType`, `EventType`, `TaskState`, `SecretScope`, etc.
- `thiserror`-based error enum with 20+ distinct variants

**Concerns:**
- `pub use ids::*;` glob re-export — could accidentally pull in too many symbols
- `pub use schedule::*;` also globs — prefer explicit re-exports for clarity

---

### `agentos-kernel` — Central Orchestrator

**Purpose:** The brain of AgentOS — orchestrates all subsystems, handles commands, runs the inference loop.

**Strengths:**
- Supervised run loop with 9 typed `TaskKind` tasks, crash restart with budget (5 restarts/60s)
- CancellationToken-based graceful shutdown
- Prometheus metrics + `/health`, `/ready` endpoints
- Per-connection rate limiting (50 cmd/s)
- Comprehensive command routing (~50 matched variants)
- Event-driven architecture with HMAC-signed events

**Concerns:**

> [!WARNING]
> **Kernel God Struct:** The `Kernel` struct holds **~35 `Arc<...>` fields** plus 5 channel receivers. The `boot()` method is **350 lines** of sequential initialization. This makes the kernel difficult to test in isolation, hard to refactor, and creates cognitive overhead.

- `run_loop.rs` at **961 lines** should be decomposed — the `TimeoutChecker` branch alone exceeds 200 lines of inline logic for escalation sweep
- When a JoinSet task panics, the restart logic **cannot identify which task crashed** (logged as `"unknown_panic"`), so it restarts all 9 tasks — sledgehammer approach
- `handle_command()` is a monolithic `match` with ~50 arms — consider a command registry pattern

---

### `agentos-vault` — Encrypted Secrets

**Purpose:** AES-256-GCM encrypted secrets vault with Argon2id key derivation.

**Strengths:**
- Proper sentinel-based passphrase verification
- Proxy token pattern (VAULT_PROXY:tok_*) — tools never see plaintext secrets directly
- Scope enforcement: Global / Agent-scoped / Tool-scoped
- Emergency lockdown with `AtomicBool` + audit logging
- Single-use tokens with expiry
- `ProxyVault` newtype restricting tool access to `resolve()` only
- 7 well-targeted unit tests

**Concerns:**
- `Mutex<Connection>` — uses `std::sync::Mutex` instead of `tokio::sync::Mutex`. Since vault operations occur within async context, this can block the Tokio runtime thread pool
- `lock().unwrap()` — panics if mutex is poisoned. Consider `lock().unwrap_or_else(|e| e.into_inner())` pattern used elsewhere
- `let _ = self.audit.append(...)` — silently discards audit write failures in `set()`, `get()`, `revoke()`, `rotate()`. Consider emitting a `tracing::error!` like the kernel's `audit_log()` helper
- No vault backup/migration mechanism
- `list()` uses `unwrap()` on datetime parsing — will panic on corrupt data

---

### `agentos-capability` — Token & Permission Engine

**Purpose:** Issues HMAC-SHA256 signed capability tokens and validates permissions.

**Strengths:**
- Constant-time HMAC verification prevents timing attacks
- Token encompasses: task, agent, tools, intents, permissions, timestamps + signature
- Per-permission time-based expiry
- Boot-time key persistence in vault (survives kernel restarts)
- Poisoned lock recovery in all write paths
- 4 focused tests covering: issue/verify, tampered token, expired token, permission denied

**Concerns:**
- No token revocation list — once issued, tokens are valid until expiry
- Signing key is stored as hex string in vault with `SecretScope::Global` — should use a kernel-only scope
- `validate_intent()` linear scans `token.permissions.entries` — O(n) per permission check. Fine for small sets, but could use a HashMap for larger permission sets

---

### `agentos-llm` — Multi-Provider LLM Adapters

**Purpose:** Abstraction layer over 5 LLM providers + mock for testing.

**Strengths:**
- Clean `LLMCore` trait with `infer()`, `infer_stream()`, `health_check()`, `capabilities()`
- Default streaming fallback for providers that don't support native SSE
- Separate modules per provider — easy to add new providers
- `ModelCapabilities` struct for context window negotiation

**Concerns:**
- No retry/backoff logic in individual adapters — network errors are propagated raw
- No circuit breaker pattern for failing LLM backends
- `async_trait` macro — consider removing once Rust's native async traits are stable (2024 edition)
- Missing timeout configuration per provider — relies on global reqwest timeout

---

### `agentos-tools` — Built-in Tool Implementations

**Purpose:** 25+ tool implementations with a shared `AgentTool` trait.

**Strengths:**
- Comprehensive `AgentTool` trait with `name()`, `description()`, `execute()`, `required_permissions()`
- Path traversal blocking in `file-reader` and `file-writer`
- File locking via `FileLockRegistry` preventing concurrent writes
- Pagination in `file-reader` with `offset`/`limit`/`has_more`
- Permission checks at tool level (not just kernel level)
- Size guard on `file-writer` preventing unbounded writes
- Manifest signing and CRL-based revocation
- **Extensive inline tests** — 30+ test cases covering security, permissions, and edge cases

**Concerns:**
- `shell-exec` tool — high-risk surface. Needs careful review of command injection protections
- Some tools accept `serde_json::Value` inputs without JSON Schema validation at the tool level
- `ToolExecutionContext` has many `Option` fields (`vault`, `hal`, `file_lock_registry`) — could use a builder pattern

---

### `agentos-bus` — IPC Layer

**Purpose:** Unix Domain Socket IPC with optional TLS support.

**Strengths:**
- Length-framed message protocol with 16MB max message size
- TLS support behind a feature flag
- Clean client/server separation
- 2 integration tests (roundtrip + large message)

**Concerns:**
- No connection pooling on the client side
- No authentication handshake — relies entirely on Unix socket permissions
- The 16MB limit is sufficient but not configurable

---

### `agentos-sandbox` — Seccomp-BPF Sandbox

**Purpose:** Process isolation for tool execution using Linux seccomp.

**Strengths:**
- Platform-conditional compilation (`#[cfg(target_os = "linux")]`)
- Configurable `SandboxConfig` with timeout, allowed syscalls
- Timeout-based kill mechanism

**Concerns:**
- Linux-only — no fallback sandbox for macOS/Windows development
- Small crate (931 LoC) — verify completeness of syscall allowlist

---

### `agentos-memory` — Three-Tier Memory

**Purpose:** Episodic, semantic, and procedural memory stores.

**Strengths:**
- Hybrid search (vector + FTS5) with Reciprocal Rank Fusion
- Chunks-based semantic memory with fastembed embeddings
- Procedural memory for learned workflows
- Shared embedder across stores (single model load)

**Concerns:**
- SQLite for vector search — acceptable at current scale, but will need a specialized vector DB for production workloads
- `Embedder` model download at boot time — first-run latency

---

### `agentos-pipeline` — Multi-Agent Pipelines

**Purpose:** DAG-based multi-agent pipeline execution.

**Strengths:**
- SQLite-backed pipeline store for persistence
- Pipeline engine with step orchestration

**Concerns:**
- Relatively new crate (1,810 LoC) — needs more test coverage

---

### `agentos-audit` — Append-Only Audit Log

**Purpose:** Immutable audit trail for all kernel operations.

**Strengths:**
- SQLite WAL mode for concurrent read/write
- Chain verification via sequence hashing
- Rich `AuditEntry` with trace ID, severity, reversibility
- Typed `AuditEventType` enum covering ~20 event categories

**Concerns:**
- No log rotation — SQLite file grows unbounded
- No export mechanism (CSV, JSON stream)

---

### Remaining Crates

| Crate | LoC | Status |
|---|---:|---|
| `agentos-hal` | 1,357 | 7 HAL drivers (system, process, network, GPU, storage, sensor, log reader) |
| `agentos-web` | 799 | Axum-based web UI — early stage |
| `agentos-wasm` | 284 | Wasmtime 38 integration — minimal but functional |
| `agentos-sdk-macros` | 214 | Proc macros for tool authoring |
| `agentos-sdk` | 101 | Public SDK API — very early stage |

---

## 4. Security Review

### ✅ Implemented Well

| Security Control | Implementation | Rating |
|---|---|---|
| **Secret Storage** | AES-256-GCM + Argon2id vault, zero-exposure proxy | ⭐⭐⭐⭐⭐ |
| **Capability Tokens** | HMAC-SHA256, constant-time verify, scoped permissions | ⭐⭐⭐⭐⭐ |
| **Audit Logging** | Append-only SQLite, chain verification, typed severity | ⭐⭐⭐⭐ |
| **Path Traversal Protection** | Canonical path checks in file tools | ⭐⭐⭐⭐ |
| **Permission Model** | Per-agent, per-resource rwx with time-based expiry | ⭐⭐⭐⭐ |
| **Seccomp Sandbox** | BPF syscall filtering for tool execution | ⭐⭐⭐⭐ |
| **Secret Scope Enforcement** | Global/Agent/Tool scoping with proxy tokens | ⭐⭐⭐⭐ |
| **Emergency Lockdown** | Atomic vault lockdown with token revocation | ⭐⭐⭐⭐ |
| **Injection Scanner** | Prompt injection detection module | ⭐⭐⭐ |
| **Tool Signing + CRL** | Ed25519 manifest signing with revocation list | ⭐⭐⭐⭐ |

### ⚠️ Areas of Concern

1. **Rate Limiting** — Per-connection (50/s) but no per-agent or per-IP limiting
2. **HMAC Key Scope** — Signing key stored as `SecretScope::Global`; should be kernel-only
3. **Shell Exec Tool** — Direct shell command execution is inherently high-risk
4. **No Input Schema Validation at Tool Level** — Tools receive `serde_json::Value`; JSON Schema is registered but not enforced at every tool boundary
5. **Audit Error Suppression** — `let _ = self.audit.append(...)` in vault — audit failures are silenced
6. **Model Allowlist** — `connected LLM models should be validated against a model allowlist` (from past conversations)

---

## 5. Error Handling

### Strengths
- Centralized `AgentOSError` enum with 20+ typed variants using `thiserror`
- Structured error messages with context (tool name, resource, operation)
- `From<std::io::Error>` auto-conversion
- Errors do not expose internal details in CLI responses

### Concerns

> [!WARNING]
> **Inconsistent `unwrap()` usage:** Multiple `Mutex::lock().unwrap()` calls exist in `SecretsVault`. If any thread panics while holding the mutex, subsequent calls will panic cascade.

- `vault.rs:156` — `self.conn.lock().unwrap()` in `set()`
- `vault.rs:193` — `self.conn.lock().unwrap()` in `get()`
- `vault.rs:251` — `.unwrap()` on datetime parsing in `list()` — will panic on corrupt data
- Capability engine properly recovers from poisoned locks (`.unwrap_or_else(|e| e.into_inner())`); vault does not

---

## 6. Performance & Concurrency

### Strengths
- Tokio multi-threaded async runtime
- `Arc<RwLock<...>>` for concurrent read-heavy access (agent registry, tool registry, active LLMs)
- CancellationToken for cooperative shutdown
- Connection-level rate limiting
- Bounded restart budgets prevent crash loops

### Concerns

1. **`std::sync::Mutex` in Vault** — The vault uses `std::sync::Mutex<Connection>` within an async runtime. This will block Tokio worker threads during SQLite operations. Should use `tokio::sync::Mutex` or `spawn_blocking()`.

2. **Unbounded Channels** — Event system uses `tokio::sync::mpsc::unbounded_channel()`. Under heavy event load, this can grow without bounds:
   - `event_sender` / `event_receiver`
   - `tool_lifecycle_sender` / `tool_lifecycle_receiver`
   - `comm_notif_sender` / `comm_notif_receiver`
   - `schedule_notif_sender` / `schedule_notif_receiver`

3. **JoinSet Panic Recovery** — When a task panics in `JoinSet`, the kernel cannot identify which task crashed and restarts **all 9 tasks**. This is wasteful and could cause transient errors.

4. **Linear Permission Lookups** — `validate_intent()` linear scans permission entries. Fine for small sets but should be indexed for large permission sets.

5. **No Connection Pooling** — Bus client creates a new connection per CLI invocation. For programmatic use, a connection pool would be needed.

---

## 7. Testing

### Statistics

| Metric | Count |
|---|---:|
| Total `#[test]` declarations | 270 |
| Async tests (`#[tokio::test]`) | 139 |
| Sync tests | 131 |
| Integration test files (`crates/agentos-cli/tests/`) | 5 |
| Top-level `tests/` directory | Empty |

### Test Coverage by Crate

| Crate | Tests | Coverage Quality |
|---|---|---|
| `agentos-tools` | ~30 inline tests | ⭐⭐⭐⭐⭐ — Path traversal, permissions, locking, pagination |
| `agentos-vault` | 7 tests | ⭐⭐⭐⭐ — Init, CRUD, scope enforcement, lockdown |
| `agentos-capability` | 4 tests | ⭐⭐⭐⭐ — Issue/verify, tamper detection, expiry, permission denied |
| `agentos-cli` | 10+ parse tests + 5 integration files | ⭐⭐⭐⭐ — CLI parsing, kernel boot, pipelines |
| `agentos-bus` | 2 tests | ⭐⭐⭐ — Roundtrip + large message |
| `agentos-kernel` | Embedded tests in subsystems | ⭐⭐⭐ — Individual subsystems tested, kernel boot integration exists |
| `agentos-memory` | Inline tests | ⭐⭐⭐ — Write/search, hybrid search |
| `agentos-pipeline` | Unknown | ⭐⭐ — Needs review |
| `agentos-hal` | Minimal | ⭐⭐ — Driver tests needed |

### Gaps

> [!CAUTION]
> 1. **No E2E test framework** — The `tests/` directory is empty
> 2. **No CI pipeline visible** — `.github/` exists but no Actions workflow YAML found
> 3. **No code coverage tracking** — No `tarpaulin`, `llvm-cov`, or similar configured
> 4. **Limited concurrency tests** — Most tests are sequential; concurrent access patterns untested

---

## 8. Configuration & Deployment

### Configuration Architecture

- **Two TOML configs:** `config/default.toml` (dev) and `config/production.toml` (deploy)
- **Env var overrides:** `AGENTOS_OLLAMA_HOST`, `AGENTOS_LLM_URL`, `AGENTOS_OPENAI_BASE_URL`
- **Startup validation:** Warns on `/tmp` paths in production
- **All config externalized** — no hardcoded URLs or endpoints

### Dev Config Defaults

> [!NOTE]
> Default config uses `/tmp/agentos/*` paths. The `warn_on_tmp_paths()` function correctly alerts when these are used in production.

### Deployment Readiness

| Item | Status |
|---|---|
| Persistent storage paths | ✅ `production.toml` exists |
| Health endpoint (`/health`, `/ready`) | ✅ Implemented |
| Prometheus metrics | ✅ `/metrics` endpoint |
| Graceful shutdown | ✅ CancellationToken + SIGTERM |
| TLS support | ✅ Optional TCP+TLS transport |
| `.env` protection | ✅ `.gitignore` covers `.env*` |
| Docker support | ❌ No Dockerfile found |
| CI/CD pipeline | ❌ No workflow YAML found |
| Log rotation | ❌ SQLite audit grows unbounded |

---

## 9. Code Quality & Maintainability

### Naming & Style

| Aspect | Rating | Notes |
|---|---|---|
| File naming | ⭐⭐⭐⭐ | Consistent `snake_case` throughout |
| Module structure | ⭐⭐⭐⭐ | Clean `lib.rs` re-exports per crate |
| Type naming | ⭐⭐⭐⭐⭐ | Descriptive: `CapabilityToken`, `ProxyVault`, `EscalationManager` |
| Function naming | ⭐⭐⭐⭐ | Clear: `issue_proxy_token()`, `sweep_expired()`, `validate_intent()` |
| Error messages | ⭐⭐⭐⭐ | Contextual and descriptive |
| Comments | ⭐⭐⭐⭐ | Doc comments on public APIs; inline comments explain "why" |

### Code Organization

**Strengths:**
- Clean crate-level separation of concerns
- `lib.rs` files serve as public API surfaces
- Internal modules are pub-crate where applicable
- Good use of type aliases and re-exports

**Concerns:**
- **Kernel monolith:** `agentos-kernel` at 20,915 LoC is nearly half the codebase. Consider splitting into `agentos-kernel-core` (data structures, config) and `agentos-kernel-runtime` (run loop, task execution).
- **Commands module size:** `agentos-kernel/src/commands.rs` likely contains all ~50 command handlers in one file — should be split by domain
- **No Rustfmt/Clippy CI enforcement** — No evidence of automated formatting/linting in CI

### Dependency Management

- All workspace dependencies centralized in root `Cargo.toml` — excellent
- `Cargo.lock` committed — correct for a binary project
- Dependency versions pinned — good
- No `unsafe` code detected in reviewed files — excellent
- `wasmtime 38` is current; `rusqlite 0.31` with bundled SQLite is appropriate

---

## 10. Documentation

| Document | Status |
|---|---|
| `README.md` | ⭐⭐⭐⭐⭐ — Comprehensive with architecture diagram, quick start, CLI reference |
| `docs/guide/` | ⭐⭐⭐⭐ — 7 guides covering intro through config |
| API docs (Rustdoc) | ⭐⭐⭐ — Doc comments on public APIs, but no generated `cargo doc` output |
| `CLAUDE.md` | ⭐⭐⭐ — Agent-specific development context (~19KB) |
| Inline comments | ⭐⭐⭐⭐ — Explain design rationale, especially in vault and capability engine |
| ADRs | ❌ — No Architecture Decision Records in `docs/` |
| Changelog | ❌ — No CHANGELOG.md |

---

## 11. Risk Register & Technical Debt

### Critical

| # | Risk | Impact | Affected Code |
|---|---|---|---|
| R1 | Kernel struct monolith (~35 Arc fields) | Hard to test/refactor | `kernel.rs` |
| R2 | `std::sync::Mutex` in async vault | Thread pool blocking | `vault.rs` |
| R3 | Unbounded event channels | Memory exhaustion | `run_loop.rs` |
| R4 | No CI/CD pipeline | Regressions go undetected | `.github/` |
| R5 | Audit error suppression in vault | Security events silently lost | `vault.rs` |

### High

| # | Risk | Impact | Affected Code |
|---|---|---|---|
| R6 | Empty `tests/` directory | No E2E validation | `tests/` |
| R7 | No Docker deployment | Blocks production use | root |
| R8 | No log/audit rotation | Disk exhaustion over time | `audit/log.rs` |
| R9 | Panic on corrupt vault dates | Vault `list()` crashes | `vault.rs:251` |
| R10 | Signing key scope too broad | Any agent can potentially access | `capability/engine.rs` |

### Medium

| # | Risk | Impact | Affected Code |
|---|---|---|---|
| R11 | No LLM retry/circuit breaker | Cascading failures | `agentos-llm` |
| R12 | JoinSet panic restarts all tasks | Unnecessary service disruption | `run_loop.rs` |
| R13 | Linear permission lookups | Performance at scale | `capability/engine.rs` |
| R14 | No code coverage tracking | Unknown test coverage | workspace |
| R15 | Pipeline crate undertested | Pipeline failures undetected | `agentos-pipeline` |

---

## 12. Recommendations

### Phase 1 — Critical (Immediate)

1. **Decompose `Kernel` struct** — Extract subsystem groups into `KernelSubsystems` or domain-specific structs (e.g., `MemorySubsystem`, `SecuritySubsystem`). Use a builder pattern for boot.

2. **Replace `std::sync::Mutex` in vault with `tokio::sync::Mutex`** or wrap operations in `spawn_blocking()` to avoid blocking the Tokio thread pool.

3. **Set up CI pipeline** — Add `.github/workflows/ci.yml` with:
   ```yaml
   - cargo check --workspace
   - cargo clippy --workspace -- -D warnings
   - cargo test --workspace
   - cargo fmt --all -- --check
   ```

4. **Fix audit error suppression** — Replace `let _ = self.audit.append(...)` in vault with the kernel's `audit_log()` helper pattern that emits `tracing::error!` on failure.

5. **Use bounded channels** for event system — Replace `unbounded_channel()` with `channel(capacity)` to prevent memory exhaustion.

### Phase 2 — High Priority

6. **Add E2E test suite** — Create `tests/e2e/` with kernel boot → agent connect → task run → tool exec → result validation flow.

7. **Add Docker deployment** — Create multi-stage `Dockerfile` with non-root user, health check, and `docker-compose.yml` for development.

8. **Implement audit log rotation** — Add configurable max size/age with archival to compressed files.

9. **Handle poisoned mutexes in vault** — Apply the `unwrap_or_else(|e| e.into_inner())` pattern already used in the capability engine.

10. **Narrow signing key scope** — Change HMAC signing key scope from `SecretScope::Global` to a kernel-only scope that agents cannot access.

### Phase 3 — Medium Priority

11. **Add LLM retry logic** — Implement exponential backoff + jitter with circuit breaker per provider.
12. **Index permission lookups** — Use `HashMap<String, PermissionEntry>` in `PermissionSet` for O(1) checks.
13. **Add code coverage** — Integrate `cargo-tarpaulin` or `cargo-llvm-cov` in CI.
14. **Create ADRs** — Document key architecture decisions in `docs/adr/`.
15. **Add CHANGELOG.md** — Track version history for contributors and users.

---

> **Overall Assessment:** AgentOS is a technically impressive project with strong security fundamentals and a well-thought-out architecture. The main risk is the kernel monolith — as the project grows, decomposing the kernel and establishing CI/CD will be critical to maintaining velocity and reliability. The security model is among the most thorough seen in AI agent frameworks, and the codebase quality is high for an actively developed v0.1.0 project.
