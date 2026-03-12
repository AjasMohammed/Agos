---
title: "Phase 2: Infrastructure Review"
tags:
  - review
  - security
  - infrastructure
  - phase-2
date: 2026-03-13
status: planned
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
- [ ] All SQL uses parameterized queries (`?1` syntax), no `format!()` in queries
- [ ] SQLite WAL mode or appropriate journaling for concurrent access
- [ ] Append-only constraint: no UPDATE/DELETE on audit entries
- [ ] Timestamps use UTC consistently
- [ ] Large payload handling (oversized entries truncated or rejected?)
- [ ] Database initialization handles existing schema (migrations)
- [ ] Connection pooling or single-connection safety

---

## Step 2.2 — Vault (~880 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-vault/src/lib.rs` (6), `crypto.rs` (43), `master_key.rs` (47), `vault.rs` (784)

**Checklist:**
- [ ] AES-256-GCM nonce never reused with same key (random vs counter strategy)
- [ ] Argon2id params: salt >= 16B, time >= 2, memory >= 64MB, parallelism >= 1
- [ ] `ZeroizingString` for master key; derived keys zeroed after use
- [ ] No plaintext secrets in Debug/Display/logs/errors
- [ ] SQL parameterized
- [ ] Vault lock/unlock lifecycle sound (no use-after-lock)
- [ ] Error messages do not leak cryptographic details

---

## Step 2.3 — LLM: Trait & Types (~303 lines)

**Files:**
- `crates/agentos-llm/src/lib.rs` (20), `traits.rs` (45), `types.rs` (238)

**Checklist:**
- [ ] `LLMCore` trait is object-safe (`Send + Sync`)
- [ ] `InferenceResult` captures token counts for cost tracking
- [ ] Stream types handle cancellation correctly
- [ ] Error types capture provider-specific errors without leaking API keys

---

## Step 2.4 — LLM: Ollama & OpenAI (~565 lines)

**Files:**
- `crates/agentos-llm/src/ollama.rs` (330), `openai.rs` (235)

**Checklist:**
- [ ] API keys not logged or included in error messages
- [ ] HTTP timeouts configured (no unbounded waits)
- [ ] Response parsing handles malformed JSON gracefully
- [ ] Base URL is configurable (not hardcoded)
- [ ] TLS certificate validation is not disabled
- [ ] Streaming responses handle partial chunks and connection drops

---

## Step 2.5 — LLM: Anthropic, Gemini, Custom, Mock (~748 lines)

**Files:**
- `crates/agentos-llm/src/anthropic.rs` (226), `gemini.rs` (239), `custom.rs` (172), `mock.rs` (111)

**Checklist:**
- [ ] Same security checks as Step 2.4 for each adapter
- [ ] Mock adapter provides deterministic behavior for tests
- [ ] Custom adapter validates its configuration
- [ ] Anthropic adapter handles `tool_use` response format correctly
- [ ] Gemini adapter maps between Google's API format and internal types

---

## Step 2.6 — Sandbox (~934 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-sandbox/src/lib.rs` (~10), `config.rs`, `filter.rs`, `executor.rs`, `result.rs`

**Checklist:**
- [ ] Seccomp BPF filter: syscall allowlist is minimal (no open-ended `allow_all`)
- [ ] `unsafe` code in `executor.rs`: verify soundness, no UB, proper error handling
- [ ] Platform gating: `#[cfg(target_os = "linux")]` used consistently
- [ ] Sandbox escape: child process cannot manipulate parent's state
- [ ] Resource limits (rlimit) applied before exec
- [ ] File descriptor inheritance is restricted
- [ ] Signal handling in sandboxed processes

---

## Step 2.7a — HAL: Core & Registry (~541 lines)

**Files:**
- `crates/agentos-hal/src/lib.rs` (8), `types.rs` (85), `hal.rs` (114), `registry.rs` (327), `drivers/mod.rs` (7)

**Checklist:**
- [ ] Driver trait is object-safe
- [ ] Registry handles duplicate driver registration
- [ ] No raw system calls that bypass sandbox
- [ ] Error handling for unavailable hardware (graceful degradation)

---

## Step 2.7b — HAL: Individual Drivers (~659 lines)

**Files:**
- `crates/agentos-hal/src/drivers/system.rs` (109), `process.rs` (144), `network.rs` (57), `gpu.rs` (123), `storage.rs` (107), `sensor.rs` (97), `log_reader.rs` (179)

**Checklist:**
- [ ] Process driver does not allow arbitrary process spawning without permission
- [ ] Network driver does not expose SSRF vectors
- [ ] Storage driver validates paths (no traversal)
- [ ] GPU driver handles missing GPU gracefully

---

## Step 2.8 — Memory (~1,581 lines, split into 2 sub-steps)

**Step 2.8a — Types, Embedder, Semantic** (~932 lines)
- `crates/agentos-memory/src/lib.rs` (9), `types.rs` (95), `embedder.rs` (123), `semantic.rs` (705)

**Step 2.8b — Episodic** (~649 lines)
- `crates/agentos-memory/src/episodic.rs` (649)

**Checklist:**
- [ ] SQL queries are parameterized (both stores use rusqlite)
- [ ] Embedding vectors normalized before cosine similarity
- [ ] Memory retrieval bounds results (no unbounded SELECT)
- [ ] SQLite threading mode correct for concurrent access
- [ ] Memory cleanup/expiry logic correct
- [ ] In-memory tier does not grow unbounded

---

## Step 2.9 — Pipeline (~1,781 lines, split into 3 sub-steps)

**Step 2.9a — Definitions & Types** (~194 lines)
- `crates/agentos-pipeline/src/lib.rs` (9), `types.rs` (98), `definition.rs` (87)

**Step 2.9b — Engine** (~1,051 lines)
- `crates/agentos-pipeline/src/engine.rs` (1,051)

**Step 2.9c — Store** (~536 lines)
- `crates/agentos-pipeline/src/store.rs` (536)

**Checklist:**
- [ ] Pipeline DAG execution handles cycles (detection or prevention)
- [ ] Step failure propagation: does one step's failure correctly cancel downstream?
- [ ] Concurrent step execution is safe (no data races between steps)
- [ ] Pipeline store SQL parameterized
- [ ] Timeout handling per step and per pipeline
- [ ] Resource cleanup on pipeline cancellation

---

## Files Changed

No files changed — read-only review phase.

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
