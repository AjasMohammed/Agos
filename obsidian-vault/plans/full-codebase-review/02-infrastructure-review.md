---
title: "Phase 2: Infrastructure Review"
tags:
  - review
  - security
  - infrastructure
  - phase-2
date: 2026-03-13
status: complete
effort: 6h
priority: critical
---

# Phase 2: Infrastructure Review

> Review Layer 1 crates: audit, vault, LLM adapters, sandbox, HAL, memory, and pipeline.

---

## Why This Phase

Layer 1 contains the most security-critical infrastructure: the encrypted vault (AES-256-GCM), the seccomp sandbox, and the audit log. It also contains all LLM adapter code (API key handling, HTTP clients) and the memory/pipeline stores (SQLite queries). Bugs here mean: leaked secrets, sandbox escapes, SQL injection, or lost audit integrity.

---

## Current → Target State

- **Current:** 7 crates, ~9,944 lines, minimal test coverage (no dedicated test files for any of these crates)
- **Target:** All files reviewed for SQL safety, crypto correctness, API key hygiene, and concurrency

---

## Step 2.1 — Audit Log (~795 lines) `SECURITY`

**Files:**
- `crates/agentos-audit/src/lib.rs` (~11 lines)
- `crates/agentos-audit/src/log.rs` (~784 lines)

**Checklist:**
- [x] All SQL uses parameterized queries (`?1` syntax), no `format!()` in queries
- [x] SQLite WAL mode or appropriate journaling for concurrent access — `PRAGMA journal_mode=WAL` in `open()`
- [x] Append-only constraint: no UPDATE/DELETE on audit entries
- [x] Timestamps use UTC consistently — `chrono::Utc::now()`
- [x] Large payload handling — `MAX_DETAILS_BYTES = 64 KiB` limit in `append()`
- [x] Database initialization handles existing schema — migration for `prev_hash`/`reversible` columns
- [x] Connection pooling or single-connection safety — `Mutex<Connection>`, single writer

---

## Step 2.2 — Vault (~880 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-vault/src/lib.rs` (6), `crypto.rs` (43), `master_key.rs` (47), `vault.rs` (784)

**Checklist:**
- [x] AES-256-GCM nonce never reused — fresh random 96-bit nonce per `encrypt()` call
- [x] Argon2id params: 32-byte salt, 3 iterations, 64 MiB memory, 1 lane — compliant
- [x] `ZeroizingString` / `ZeroizeOnDrop` on `MasterKey` and passphrase
- [x] No plaintext secrets in errors — error messages are generic ("Invalid passphrase", not the key)
- [x] SQL parameterized — all queries use `params![]`
- [x] Vault lock/unlock lifecycle — `AtomicBool locked_down` + proxy token TTL enforced
- [x] Error messages do not leak crypto details

---

## Step 2.3 — LLM: Trait & Types (~303 lines)

**Files:**
- `crates/agentos-llm/src/lib.rs` (20), `traits.rs` (45), `types.rs` (238)

**Checklist:**
- [x] `LLMCore` trait is object-safe — `#[async_trait]`, `Send + Sync` bounds via `Arc<dyn LLMCore>`
- [x] `InferenceResult` captures token counts (`TokenUsage` with prompt/completion/total)
- [x] Stream types handle cancellation — `InferenceStream` uses channels; N/A, streaming not yet wired
- [x] Error types do not leak API keys — `SecretString` from `secrecy` crate guards all keys

---

## Step 2.4 — LLM: Ollama & OpenAI (~565 lines)

**Files:**
- `crates/agentos-llm/src/ollama.rs` (330), `openai.rs` (235)

**Checklist:**
- [x] API keys not logged — `SecretString`/`ExposeSecret` only at send time; errors use `reqwest::Error` not key
- [x] HTTP timeouts — `connect_timeout(10s)`, `timeout(120s)` on all clients
- [x] Response parsing handles malformed JSON — `.map_err(|e| AgentOSError::LLMError {...})`
- [x] Base URL configurable — OpenAI: `with_base_url()`; Ollama: `host` param
- [x] TLS not disabled — no `danger_accept_invalid_certs` calls
- [x] Streaming — not yet wired end-to-end; `infer_stream` trait method absent (acceptable for current scope)

---

## Step 2.5 — LLM: Anthropic, Gemini, Custom, Mock (~748 lines)

**Files:**
- `crates/agentos-llm/src/anthropic.rs` (226), `gemini.rs` (239), `custom.rs` (172), `mock.rs` (111)

**Checklist:**
- [x] Same security checks as Step 2.4 for each adapter — all pass
- [x] **FIXED**: Anthropic `base_url` now configurable via `with_base_url()` (was hardcoded)
- [x] Mock adapter provides deterministic responses — ordered queue + fallback
- [x] Custom adapter validates its configuration — checked at build time
- [x] Anthropic handles content blocks — iterates `content` array, extracts `text` blocks
- [x] Gemini maps API format — separate `format_contents()` method with role mapping

---

## Step 2.6 — Sandbox (~934 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-sandbox/src/lib.rs` (~10), `config.rs`, `filter.rs`, `executor.rs`, `result.rs`

**Checklist:**
- [x] Seccomp BPF allowlist model — `SeccompAction::Errno(EPERM)` for all unlisted syscalls
- [x] `unsafe` in `executor.rs` — `pre_exec` only calls async-signal-safe libc functions (setrlimit, prctl)
- [x] Platform gating — `#[cfg(target_os = "linux")]` used on seccomp + close_range paths
- [x] Sandbox escape — `close_range(3, MAX_FD, 0)` closes inherited FDs; `env_clear()` scrubs env
- [x] Resource limits — RLIMIT_AS, RLIMIT_CPU, RLIMIT_NPROC, RLIMIT_FSIZE, RLIMIT_NOFILE all set
- [x] FD inheritance restricted — stdin=null, stdout/stderr=piped, all others closed via close_range
- [x] **FIXED**: Temp file names now use `Uuid::new_v4()` (was non-cryptographic SystemTime+PID)
- [x] **FIXED**: stdout/stderr reading capped at 10 MiB (was unbounded `read_to_string`)

---

## Step 2.7a — HAL: Core & Registry (~541 lines)

**Files:**
- `crates/agentos-hal/src/lib.rs` (8), `types.rs` (85), `hal.rs` (114), `registry.rs` (327), `drivers/mod.rs` (7)

**Checklist:**
- [x] Driver trait is object-safe — `Arc<dyn HardwareDriver + Send + Sync>`
- [x] Registry handles duplicate driver registration — returns `AlreadyRegistered` error
- [x] No raw syscalls that bypass sandbox — HAL drivers use safe Rust std/sysinfo APIs
- [x] Error handling for unavailable hardware — all drivers return `Result<_, AgentOSError>`

---

## Step 2.7b — HAL: Individual Drivers (~659 lines)

**Files:**
- `crates/agentos-hal/src/drivers/system.rs` (109), `process.rs` (144), `network.rs` (57), `gpu.rs` (123), `storage.rs` (107), `sensor.rs` (97), `log_reader.rs` (179)

**Checklist:**
- [x] Process driver — list/status only; no spawn; kill gated behind HAL permission
- [x] Network driver — interface info only; no outbound connections exposed
- [x] Storage driver — path validation present; `..` traversal checked at tool layer
- [x] GPU driver — returns empty/unknown gracefully when no GPU present

---

## Step 2.8 — Memory (~1,581 lines, split into 2 sub-steps)

**Step 2.8a — Types, Embedder, Semantic** (~932 lines)
- `crates/agentos-memory/src/lib.rs` (9), `types.rs` (95), `embedder.rs` (123), `semantic.rs` (705)

**Step 2.8b — Episodic** (~649 lines)
- `crates/agentos-memory/src/episodic.rs` (649)

**Checklist:**
- [x] SQL queries parameterized — all `params![]` in both semantic and episodic stores
- [x] Embedding vectors — cosine similarity computed; vectors come from the same model so normalized implicitly; dimension mismatch check skips mismatched chunks
- [x] Retrieval bounded — `FTS_CANDIDATE_LIMIT=200`, `RECENCY_FALLBACK_LIMIT=500`; episodic queries all take explicit `limit`
- [x] Threading — `Mutex<Connection>` (episodic), `Arc<Mutex<Connection>>` (semantic) — single-writer safe
- [x] Memory cleanup — `sweep_old_entries(max_age)` in episodic; no TTL-based sweep in semantic (acceptable)
- [x] **FIXED**: SemanticStore `write()` now uses rusqlite `Transaction` (was fragile manual BEGIN/ROLLBACK/COMMIT)

---

## Step 2.9 — Pipeline (~1,781 lines, split into 3 sub-steps)

**Step 2.9a — Definitions & Types** (~194 lines)
- `crates/agentos-pipeline/src/lib.rs` (9), `types.rs` (98), `definition.rs` (87)

**Step 2.9b — Engine** (~1,051 lines)
- `crates/agentos-pipeline/src/engine.rs` (1,051)

**Step 2.9c — Store** (~536 lines)
- `crates/agentos-pipeline/src/store.rs` (536)

**Checklist:**
- [x] DAG cycle detection — Kahn's algorithm; `sorted.len() != steps.len()` → `CircularDependency` error
- [x] Step failure propagation — `OnFailure::Fail` returns immediately; downstream steps never queued
- [x] Concurrent step execution — sequential (no parallel step dispatch); no data races possible
- [x] Pipeline store SQL parameterized — all `rusqlite::params![]`
- [x] Per-step timeout — `step.timeout_minutes` + `tokio::time::timeout`
- [x] Resource cleanup on cancellation — `PipelineRun` status set to `Failed`; store updated

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-sandbox/Cargo.toml` | Add `uuid` workspace dependency |
| `crates/agentos-sandbox/src/executor.rs` | Replace `uuid_v4_hex()` (non-CSPRNG) with `Uuid::new_v4()`; cap stdout/stderr at 10 MiB |
| `crates/agentos-llm/src/anthropic.rs` | Add `base_url` field + `with_base_url()` constructor |
| `crates/agentos-memory/src/semantic.rs` | Replace manual `BEGIN/ROLLBACK/COMMIT` with rusqlite `Transaction` |

## Dependencies

Phase 1 (foundation types understood).

## Verification

```bash
cargo build -p agentos-audit -p agentos-vault -p agentos-llm -p agentos-sandbox -p agentos-hal -p agentos-memory -p agentos-pipeline
cargo test -p agentos-audit -p agentos-vault -p agentos-llm -p agentos-memory -p agentos-pipeline
```

---

## Related

- [[Full Codebase Review Plan]]
- [[01-foundation-types-review]]
- [[03-bus-and-capability-review]]
