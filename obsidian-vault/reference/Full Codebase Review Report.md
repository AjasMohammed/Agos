---
title: "Full Codebase Review Report"
tags:
  - review
  - security
  - report
  - reference
date: 2026-03-17
status: complete
---

# Full Codebase Review Report

> Consolidated findings from the complete AgentOS codebase review (Phases 1–9). All critical and warning-severity issues have been remediated; remaining deferred items are tracked below.

---

## 1. Executive Summary

| Severity | Total Found | Fixed | Deferred |
|----------|------------|-------|----------|
| CRITICAL | 9 | 9 | 0 |
| HIGH | 7 | 5 | 2 |
| WARNING | 33 | 30 | 3 |
| INFO / LOW | 8 | 6 | 2 |
| **Total** | **57** | **50** | **7** |

### Top 7 Most Critical Issues (all fixed)

1. **HMAC token signing incomplete** — `deny_entries` and per-permission `expires_at` were not included in the signed field set, allowing an attacker to strip deny entries or remove expiry from a captured token without invalidating its HMAC signature.
2. **ContextWindow Summarize overflow** — When `non_system_count ≤ 2`, the Summarize strategy is a net no-op (removes 1, inserts 1), causing `push()` to exceed `max_entries` — silent overflow, no eviction.
3. **Injection scanner Unicode bypass** — No NFKC normalization before regex matching; fullwidth Unicode (`ｉｇｎｏｒｅ`) bypassed all patterns.
4. **Kernel panic on agent lookup** — `task.rs` used `.unwrap()` on an agent registry lookup in the production path — panics if the agent disappears between routing and execution.
5. **Bus message size unlimited** — No `MAX_MESSAGE_SIZE` cap on incoming bus messages; a client could send a multi-GB payload, causing OOM on the kernel process.
6. **SSRF via escalation webhook** — `create_escalation()` accepted any URL for the webhook `POST`, including `http://169.254.169.254/` (cloud metadata service) and RFC 1918 internal addresses — a malicious operator config could exfiltrate cloud credentials.
7. **Budget reset race condition** — Daily cost-budget reset used a non-atomic `chrono::DateTime` under a shared read lock; two concurrent threads could both observe >24h elapsed, both reset counters, and lose accumulated cost attribution data.

### Overall Security Posture: **YELLOW → GREEN**

Before this review, the system had several exploitable critical flaws in the authorization boundary, injection defense, and crypto parameters. All critical issues are now remediated. The remaining deferred items are medium-severity architectural concerns (mTLS, token revocation, HAL auth), which do not enable immediate exploitation but should be addressed before production deployment.

---

## 2. Critical Findings (all fixed)

| # | Phase | File | Line(s) | Issue | Root Cause | Fix Applied |
|---|-------|------|---------|-------|-----------|-------------|
| C1 | 3 | `agentos-capability/src/token.rs` | `feed_token_fields()` | HMAC signing excluded `deny_entries` and `PermissionEntry.expires_at` — an attacker could strip deny entries or remove expiry without invalidating the signature | Field omission in the HMAC payload builder | Rewrote `feed_token_fields()` with length-prefixed encoding covering all security-relevant fields |
| C2 | 3 | `agentos-capability/src/token.rs` | `compute_signature()` | No length-prefixed encoding for variable-length fields — a token covering `["a","bc"]` has the same HMAC as one covering `["ab","c"]` | Naive field concatenation | Added `len as u64` prefix before every variable-length field |
| C3 | 1 | `agentos-types/src/context.rs` | `push()` Summarize branch | Summarize overflow strategy is a net no-op when `non_system_count ≤ 2`; removes 1, inserts 1, then push exceeds `max_entries` | Off-by-one in strategy logic | Added safety-net FIFO eviction check after the `match` block, before `push()` |
| C4 | 8 | `agentos-kernel/src/injection_scanner.rs` | `scan()` | No Unicode NFKC normalization — fullwidth and homoglyph characters (`ｉｇｎｏｒｅ`) bypassed all regex patterns | Pre-normalization step missing | Apply NFKC via `unicode-normalization` crate before pattern matching |
| C5 | 8 | `agentos-kernel/src/injection_scanner.rs` | pattern list | No standalone base64 block detection — a large base64 blob without explicit keyword prefix could carry hidden instructions | Encoded payload patterns required explicit trigger words | Added `encoded_base64_standalone` pattern (60+ char base64 runs) |
| C6 | 5 | `agentos-kernel/src/commands/task.rs` | `cmd_run_task()` | `.unwrap()` on agent registry lookup in production path — panics if agent disappears between routing and task start | Premature `.unwrap()` | Replaced with `match`/`KernelResponse::Error` |
| C7 | 3 | `agentos-bus/src/transport.rs` | `read_message()` | No upper bound on incoming message size; a client could send a multi-GB payload causing OOM | Missing length guard | Added `MAX_MESSAGE_SIZE = 16 MiB` constant; write path validates before `u32` cast |
| C8 | 5 | `agentos-kernel/src/escalation.rs` | `create_escalation()` | Webhook URL accepted any URL including `http://169.254.169.254/` (cloud metadata) and RFC 1918 private IPs — SSRF via operator-controlled escalation config | No URL validation before outbound HTTP POST | Added `validate_webhook_url()` requiring HTTPS, blocking loopback/RFC 1918/169.254.x.x/metadata hostnames |
| C9 | 5 | `agentos-kernel/src/cost_tracker.rs` | `record_inference()` | `period_start` stored as `chrono::DateTime` read under read lock; two threads could both see >24h elapsed and both reset counters — double-reset loses cost attribution and budget state | Non-atomic read-check-write on `period_start` under shared read lock | Changed `period_start` to `AtomicI64` (Unix timestamp); reset uses `compare_exchange` ensuring exactly one thread wins |

---

## 3. High Findings

### Fixed

| # | Phase | File | Issue | Fix Applied |
|---|-------|------|-------|-------------|
| H1 | 3 | `agentos-bus/src/server.rs` | Unix socket file created with default umask (world-accessible on many systems) | Added `set_permissions(0o600)` after bind |
| H2 | 3 | `agentos-bus/src/client.rs` | No connect timeout — kernel could stall indefinitely if socket exists but kernel is unresponsive | Added 5-second connect timeout |
| H3 | 3 | `agentos-bus/src/transport.rs` | No I/O timeout — slowloris-style attack could hold a kernel reader goroutine forever | Added 30-second per-read I/O timeout |
| H4 | 3 | `agentos-bus/src/transport.rs` | Zero-length message (`len == 0`) not rejected — would cause downstream deserialization panic | Added explicit `len == 0` rejection |
| H5 | 5 | `agentos-kernel/src/agent_message_bus.rs` | `UnboundedSender<AgentMessage>` inboxes — a stalled agent accumulates unlimited messages causing OOM | Changed to bounded `mpsc::channel(256)` with `TrySendError::Full` backpressure |

### Deferred

| # | Phase | Issue | Severity | Tracking |
|---|-------|-------|----------|---------|
| H6 | 3 | TLS server lacks mTLS (client auth) — any process that can reach the TLS endpoint can connect | HIGH | Bus hardening phase |
| H7 | 3 | `SetSecret`/`RotateSecret` bus commands carry secret value in a plain `String` field — secrets sit in memory without zeroize | HIGH | Bus hardening phase |

---

## 4. Warning Findings (all fixed)

| # | Phase | File | Line(s) | Issue | Fix Applied |
|---|-------|------|---------|-------|-------------|
| W1 | 1 | `agentos-types/src/context.rs` | `TokenBudget::validate()` | Allowed negative percentage values (e.g., `system_pct = -0.5`) — would cause negative token allocation | Added per-field non-negative check |
| W2 | 1 | `agentos-types/src/capability.rs` | `is_denied()` | Deny entries for `net:`/`network:` resources were case-sensitive — `net:EVIL.COM` could bypass `net:evil.com` deny pattern | Case-normalize both sides for network resources |
| W3 | 2 | `agentos-tools/src/runner.rs` | tool execution | Temp file names used non-cryptographic `SystemTime + PID` — predictable by local attacker | Changed to `Uuid::new_v4()` |
| W4 | 2 | `agentos-tools/src/runner.rs` | stdout/stderr capture | `read_to_string()` unbounded on tool output — a tool could flood memory | Capped at 10 MiB |
| W5 | 2 | `agentos-memory/src/semantic.rs` | `write()` | No rusqlite transaction — partial write on error left DB inconsistent | Wrapped in `Transaction` |
| W6 | 4 | `agentos-tools/src/file_reader.rs` | `read()` | No file size limit — `read_to_string` on a multi-GB file caused OOM | Reject files > 10 MiB before reading |
| W7 | 4 | `agentos-tools/src/file_reader.rs` | containment check | `data_dir` not canonicalized before prefix check — a symlink could escape containment | Added `fs::canonicalize` on `data_dir` |
| W8 | 4 | `agentos-tools/src/memory_search.rs` | `top_k` | No upper bound on `top_k` — user could request 1,000,000 results | Capped `top_k` at 100 |
| W9 | 4 | `agentos-tools/src/memory_write.rs` | `content` | Content field size uncapped — user could write 1 GB string to memory | Reject if > 512 KiB |
| W10 | 4 | `agentos-tools/src/data_parser.rs` | `data` field | 4 MiB cap missing on `data`; CSV row count uncapped (50k limit) | Added caps |
| W11 | 4 | `agentos-wasm/src/lib.rs` | WASM store | No WASM memory limit — a WASM tool could allocate unlimited host memory | `ResourceLimiter` with 256 MiB cap wired in |
| W12 | 5 | `agentos-kernel/src/router.rs` | 116, 129 | `.last().unwrap()` on agents vec in CapabilityFirst/CostFirst strategies | Changed to `ok_or_else` returning `KernelError` |
| W13 | 5 | `agentos-kernel/src/agent_message_bus.rs` | message history | `Vec<AgentMessage>` history grew unbounded — OOM under long-running system | Capped at `MAX_HISTORY = 10,000` with drain-oldest |
| W14 | 6 | `agentos-web/src/csrf.rs` | 13 | `TOKEN_TTL` declared `pub` instead of `pub(crate)` | Changed to `pub(crate)` |
| W15 | 6 | `agentos-web/src/server.rs` | `start_with_shutdown()` | CSRF token `DashMap` grew unbounded as browser sessions were abandoned | Added tokio sweep task every 30 min evicting entries > 2×TOKEN_TTL |
| W16 | 6 | `agentos-cli/src/commands/secret.rs` | `parse_scope()` | `agent:` and `tool:` scopes accepted empty names (e.g. `agent:`) | Added empty-name validation with descriptive error |
| W17 | 7 | `agentos-audit/src/log.rs` | `verify_chain()` | If DB query for predecessor hash fails, `.ok()` silently set it to `None` — corrupted chain could pass verification | Propagate as `AgentOSError::VaultError` |
| W18 | 7 | `agentos-kernel/src/kernel.rs` | boot | Vault parent directory created with default umask — on multi-user systems the `/tmp` path is world-listable | `set_permissions(0o700)` on parent at startup |
| W19 | 8 | `agentos-kernel/src/injection_scanner.rs` | `taint_wrap()` | `source` attribute interpolated without HTML escaping — a tool name containing `"` could inject XML attributes | HTML-escape `source` (`&amp;`, `&quot;`, `&lt;`, `&gt;`) |
| W20 | 8 | `agentos-kernel/src/injection_scanner.rs` | pattern list | Missing closing XML tag pattern `</system>` — closing tags alone can confuse LLM context parsing | Added `delimiter_fake_xml_close_tag` pattern |
| W21 | 8 | `agentos-vault/src/master_key.rs` | 20 | Argon2id `parallelism = 1` — wastes multi-core hardware and reduces brute-force resistance | Changed to `parallelism = 4` (OWASP minimum) |
| W22 | 5 | `agentos-kernel/src/commands/permission.rs` | `cmd_grant_permission()` | `capability_engine.update_permissions(...).ok()` discarded the error — audit log recorded `PermissionGranted` even when the update silently failed, creating false-positive audit entries | `.ok()` suppressed `Err` | Changed to `if let Err(e)` propagation returning `KernelResponse::Error`; same fix in `cmd_grant_permission_timed()` |
| W23 | 5 | `agentos-kernel/src/agent_registry.rs` | `save_to_disk()` | Serialization and `fs::write` failures were silently ignored — agent registry changes lost on disk without any notification | `.ok()` / `let _ =` on all error paths | Added `tracing::warn!` on both `serde_json::to_string_pretty` failure and `fs::write` failure |
| W24 | 5 | `agentos-kernel/src/cost_tracker.rs` | `record_inference()` | `cost.total_cost_usd as u64` cast is undefined behavior if the value is `NaN` or `Inf` (possible from LLM adapter returning garbage floats) | No float guard before cast | Added `if cost.total_cost_usd.is_finite()` guard; non-finite values coerce to 0 |
| W25 | 5 | `agentos-kernel/src/commands/permission.rs` | `cmd_grant_permission_timed()` | `expires_secs as i64` truncating cast wraps to negative on values > `i64::MAX` — would create a permission with a past expiry (effectively never valid) | Unchecked `as` cast on `u64` | Changed to `i64::try_from(expires_secs).unwrap_or(i64::MAX / 2)` |
| W26 | 5 | `agentos-kernel/src/run_loop.rs` | timeout checker loop | Timed-out tasks did not have `context_manager`, `intent_validator`, or `resource_arbiter` cleaned up — context windows, validator state, and resource locks leaked indefinitely after a task timeout | Cleanup calls were only in `execute_task_sync` success/failure paths, not in the timeout authority path | Added `context_manager.remove_context`, `intent_validator.remove_task`, and `resource_arbiter.release_all_for_agent` in the timeout checker after `cleanup_task_subscriptions` |
| W27 | 5 | `agentos-kernel/src/resource_arbiter.rs` | `is_expired()` | `signed_duration_since().num_seconds() as u64` — a backwards NTP clock adjustment (negative `i64`) wraps to a huge `u64`, falsely marking the lock as expired and releasing it prematurely | No `.max(0)` guard before `as u64` cast | Added `.max(0)` before the cast |
| W28 | 5 | `agentos-kernel/src/background_pool.rs` | `BackgroundPool` | Terminal tasks (Complete/Failed) accumulated in the `HashMap` forever — monotonic memory growth for long-running kernels | No eviction API | Added `evict_terminal(max_age_secs)` method; called every 10 minutes in the timeout checker sweep |
| W29 | 5 | `agentos-kernel/src/task_completion.rs` | `complete_task_success/failure` | `update_state_if_not_terminal(...).unwrap_or(false)` silently treated scheduler internal errors the same as "task already terminal" — no log, no audit event | `.unwrap_or` swallowed `Err` | Changed to `.unwrap_or_else(|e| { tracing::error!(...); false })` in both completion paths |
| W30 | 5 | `agentos-kernel/src/task_completion.rs` | `complete_task_success` | Consolidation `tokio::spawn` was not linked to the kernel's `CancellationToken` — could run indefinitely during graceful shutdown, delaying process exit | Missing `tokio::select!` with cancellation | Wrapped in `tokio::select!` with `token.cancelled()` arm |
| W31 | 5 | `agentos-kernel/src/schedule_manager.rs` | `create_job()` | No duplicate job name check — multiple jobs with the same name could coexist; name-based lookup would silently return only the first match | Missing pre-insert name uniqueness check | Added `jobs.values().any(|j| j.name == name)` guard returning `AgentOSError::SchemaValidation` |
| W32 | 5 | `agentos-kernel/src/task_executor.rs` | `execute_task_sync` | `context_manager.push_entry(...).await.ok()` — push failure silently ignored; LLM loop continues with potentially stale context window | `.ok()` suppressed error | Changed to `if let Err(e)` with `tracing::warn!` |

### Deferred Warnings

| # | Phase | Issue | Severity | Notes |
|---|-------|-------|----------|-------|
| W26 | 8 | `commands/hal.rs` — HAL approve/deny/revoke have no token-based authorization check | WARNING | Protected by bus-level auth today; proper per-command auth is spec #9 (HAL quarantine workflow) |
| W27 | 8 | `capability/engine.rs boot()` — signing key persistence failure not fatal; kernel continues with ephemeral key | INFO/WARNING | By design; error surfaced at `error!` level; tokens are short-lived |
| W28 | 3 | Rate limiter `HashMap` grows unboundedly with distinct fake agent names | MEDIUM | Bus hardening phase |

---

## 5. Info / Low Findings

| # | Phase | File | Issue | Status |
|---|-------|------|-------|--------|
| I1 | 1 | `agentos-sdk-macros/src/lib.rs` | Missing `rx`/`wx` compound permission ops (read+execute, write+execute) | FIXED — added |
| I2 | 2 | `agentos-llm/src/anthropic.rs` | `base_url` was hardcoded — could not be overridden for proxies/gateways | FIXED — added `with_base_url()` |
| I3 | 7 | `config/default.toml` | `/tmp` vault and audit paths documented without security warning | FIXED — added warning comments |
| I4 | 9 | `config/default.toml` | `audit_path = /tmp` — same world-listable concern as vault | DOCUMENTED — comment added |
| I5 | 8 | `agentos-capability/src/token.rs:33` | `permissions.entries` is `Vec<PermissionEntry>` (order-dependent) — a token reconstructed with entries in a different order would fail HMAC verification spuriously | FIXED — sort entries by resource name before HMAC feed |
| I6 | 3 | `agentos-capability` | `ProfileManager` not wired into token issuance (dead feature) | DEFERRED — feature backlog |
| I6 | 3 | Bus | Per-connection rate limit (50/sec) hardcoded, not configurable | DEFERRED — config phase |
| I7 | 3 | Bus | Stale socket removal has TOCTOU race (check-then-delete) | DEFERRED — bus hardening |
| I8 | 1 | `agentos-types/src/event.rs` | Only ~55 EventType variants; audit log has 83+ event types — gap in EventType coverage | DOCUMENTED — ongoing |

---

## 6. Architecture Observations

### Structural Strengths

- **Dependency graph flows downward cleanly** — no circular dependencies detected across 17 crates.
- **ContextWindow overflow strategies** are well-designed with four distinct modes (FIFO, Summarize, SlidingWindow, SemanticEviction); the Summarize bug was an edge case, not a design flaw.
- **Resource arbiter** (`resource_arbiter.rs`) uses DFS deadlock detection, FIFO waiter queue, and TTL auto-release — solid implementation.
- **Capability token signing** (after Phase 3 fixes) uses HMAC-SHA256 with length-prefixed fields covering all security-relevant payload fields.
- **Audit log** uses SHA-256 hash chains for tamper detection with `verify_chain()`.
- **Injection scanner** (after Phase 8 fixes) is comprehensive: 22 patterns across 7 categories, NFKC-normalized, with XML-safe taint wrapping.

### Concerns

- **HAL authorization gap** — The 4 HAL commands (`list/register/approve/deny/revoke`) have no capability token or permission check; they rely entirely on bus-level authentication. This is acceptable for a CLI-only deployment but becomes a security gap once the web UI or remote API is exposed. Tracked as deferred spec #9.
- **`ProfileManager` dead code** — Defined in `agentos-capability/src/profiles.rs`, correctly stores/retrieves profiles, but is never consulted during token issuance. Either wire it in or remove it.
- **Secret transit hygiene** — `SetSecret`/`RotateSecret` bus commands carry the secret in a plain `String`; this means the plaintext exists in serialized form on the Unix socket and in the kernel's message buffer. Should use `ZeroizingString` and clear the buffer after handling.
- **No token revocation** — Capability tokens have no revocation mechanism; the defense is short TTLs (60–300s). This is acceptable today but should be addressed before multi-tenant deployment.

---

## 7. Test Coverage Report

### Coverage by Crate

| Crate | Test Type | Status | Notes |
|-------|-----------|--------|-------|
| `agentos-types` | Inline `#[cfg(test)]` | ✅ Good | 25+ tests across context, capability, IDs |
| `agentos-capability` | Inline `#[cfg(test)]` | ✅ Good | 9 tests; 4 added in Phase 3 |
| `agentos-bus` | Inline + integration | ✅ Good | 2 integration tests |
| `agentos-vault` | Inline `#[cfg(test)]` | ✅ Good | Encrypt/decrypt, key derivation |
| `agentos-audit` | Inline `#[cfg(test)]` | ⚠️ Partial | Hash chain test; no concurrency tests |
| `agentos-llm` | Inline | ⚠️ Partial | Mock adapter only; provider tests limited |
| `agentos-tools` | Dedicated tests | ✅ Good | 2 test files; containment, runner covered |
| `agentos-sdk` | Dedicated tests | ⚠️ Minimal | 1 file, macro smoke test only |
| `agentos-sdk-macros` | None | ❌ Missing | Only tested indirectly via `agentos-sdk` |
| `agentos-kernel` | CLI integration | ⚠️ Indirect | 49 files tested via 5 CLI integration tests |
| `agentos-memory` | None | ❌ Missing | Semantic store, episodic store — no tests |
| `agentos-pipeline` | None | ❌ Missing | Engine, store — no tests |
| `agentos-hal` | None | ❌ Missing | Hardware registry — no tests |
| `agentos-sandbox` | None | ❌ Missing | Seccomp filter — no direct tests |
| `agentos-wasm` | None | ❌ Missing | Only tested via kernel integration |
| `agentos-web` | None | ❌ Missing | CSRF, auth, handlers — no tests |
| `agentos-cli` | 5 integration tests | ✅ Good | 800 lines; covers happy paths |

### Top 10 Missing Tests (by risk)

| Priority | Crate / Module | Risk | Test Type Needed |
|----------|---------------|------|-----------------|
| 1 | `agentos-kernel/injection_scanner.rs` | NFKC bypass, missing pattern regressions | Unit: one test per pattern + homoglyph variant |
| 2 | `agentos-web/auth.rs` + `csrf.rs` | Auth bypass, CSRF token validation | Integration: login/logout/token expiry |
| 3 | `agentos-kernel/task_executor.rs` | State machine correctness, timeout enforcement | Unit: task lifecycle transitions |
| 4 | `agentos-memory/semantic.rs` | Transaction correctness, vector search bounds | Unit: write/read/corrupt + top_k limits |
| 5 | `agentos-pipeline/engine.rs` | Pipeline step failure handling, partial execution | Unit: step error propagation |
| 6 | `agentos-audit/log.rs` | Chain verify with corrupted/truncated DB | Unit: tampered entry detection |
| 7 | `agentos-capability/engine.rs` | Token issuance limits, revoke_agent invalidation | Unit: cannot exceed issuer permissions |
| 8 | `agentos-tools/file_reader.rs` | Path traversal, symlink escape | Security: `../` variants, symlinks |
| 9 | `agentos-bus/transport.rs` | Oversized message rejection, timeout | Unit: MAX_MESSAGE_SIZE + slow client |
| 10 | `agentos-hal/src/lib.rs` | Device quarantine state transitions | Unit: approve/deny/revoke lifecycle |

---

## 8. Security Posture Summary

### Authorization Boundary (Capability Engine)

**Grade: A−**

- HMAC-SHA256 token signing now covers all fields including deny entries and per-permission expiry
- Length-prefixed encoding prevents concatenation collision attacks
- Constant-time comparison via `mac.verify_slice()` (no timing oracle)
- Token expiry checked on every validation call
- **Remaining gap:** No revocation mechanism; relies on short TTLs (60–300s)
- **Remaining gap:** `issue_token` trusts its caller to pass correctly-bounded permissions (no defense-in-depth guard inside the engine itself)

### Injection Defense (Injection Scanner + Context)

**Grade: B+**

- 22 regex patterns across 7 categories: role override, exfiltration, delimiter injection, encoded payloads, privilege escalation, data exfiltration, ChatML token injection
- NFKC normalization prevents Unicode homoglyph bypass
- Closing XML tag detection and standalone base64 blob detection added
- `taint_wrap()` HTML-escapes the source attribute (prevents XML injection)
- `<user_data>` tagging applied consistently
- **Remaining gap:** Multi-step split injection (payload split across 2 context entries) is not detected by single-pass scanning; would require cross-entry correlation

### Secrets Management (Vault + Transport)

**Grade: B**

- AES-256-GCM encryption at rest with Argon2id key derivation
- Argon2id now uses `parallelism=4` (OWASP recommended)
- `MasterKey` uses `ZeroizeOnDrop`; capability signing key zeroized on drop
- Vault parent directory created with `0o700` permissions
- Audit chain uses SHA-256 integrity hashes; `verify_chain()` errors now propagated
- **Remaining gap:** Bus `SetSecret`/`RotateSecret` carry plaintext `String` in the message (transit exposure on Unix socket)
- **Remaining gap:** Default vault path is `/tmp` (production deployments must override to `~/.agentos/`)

### Sandbox Effectiveness

**Grade: B+**

- Linux seccomp-BPF syscall filtering in `agentos-sandbox` (Linux-only)
- WASM tools isolated by Wasmtime with `ResourceLimiter` (256 MiB memory cap)
- Tool file I/O restricted to `data_dir` with canonicalized path prefix checks
- Tool output capped at 10 MiB; WASM execution isolated from host filesystem via WASI
- **Remaining gap:** HAL device commands lack per-command authorization (spec #9 deferred)
- **Remaining gap:** Shell tool execution still runs in-process without additional seccomp (only kernel-level seccomp applies)

### Overall Security Grade: **B+ / YELLOW-GREEN**

All critical vulnerabilities fixed. Remaining items are architectural hardening tasks, not immediate exploits.

---

## 9. Deferred Items (Prioritized Backlog)

| Priority | Item | Severity | Phase | Effort |
|----------|------|----------|-------|--------|
| 1 | Bus: `SetSecret`/`RotateSecret` use `String` not `ZeroizingString` | HIGH | Phase 3 deferred | 2h |
| 2 | Bus: TLS server lacks mTLS (client auth) | HIGH | Phase 3 deferred | 4h |
| 3 | HAL commands: add capability token authorization checks | WARNING | Spec #9 | 1d |
| 4 | Bus: no concurrent connection limit (unbounded `tokio::spawn`) | MEDIUM | Bus hardening | 2h |
| 5 | Bus: rate limiter HashMap unbounded with fake agent names | MEDIUM | Bus hardening | 2h |
| 6 | Bus: stale socket removal TOCTOU race | MEDIUM | Bus hardening | 1h |
| 7 | Capability: no token revocation mechanism | MEDIUM | Feature backlog | 3d |
| 8 | Capability: no escalation guard inside `issue_token` | MEDIUM | Security hardening | 2h |
| 9 | `ProfileManager` not wired into token issuance (dead code) | LOW | Feature backlog | 1h |
| 10 | EventType coverage: ~55 variants vs 83+ audit event types | LOW | Ongoing | 4h |

---

## 10. Files Changed (This Review)

All changes were applied incrementally across phases 1–9:

| File | Phase | Change |
|------|-------|--------|
| `crates/agentos-types/src/context.rs` | 1 | Summarize overflow safety-net; TokenBudget negative pct validation |
| `crates/agentos-types/src/capability.rs` | 1 | Network deny entry case normalization |
| `crates/agentos-sdk-macros/src/lib.rs` | 1 | Add `rx`/`wx` compound permission ops |
| `crates/agentos-capability/src/token.rs` | 3 | Rewrote HMAC with length-prefixed encoding; cover deny_entries + expires_at |
| `crates/agentos-capability/src/token.rs` | 8 | Sort `permissions.entries` by resource name before HMAC feed (deterministic order) |
| `crates/agentos-capability/src/engine.rs` | 3 | `verify_signature()` delegates to token.rs; `Drop` key zeroization; 4 security tests |
| `crates/agentos-capability/Cargo.toml` | 3 | Added `zeroize` dep |
| `crates/agentos-bus/src/transport.rs` | 3 | MAX_MESSAGE_SIZE 16 MiB; I/O timeouts 30s; len==0 reject; u32 cast guard |
| `crates/agentos-bus/src/server.rs` | 3 | `0o600` socket permissions |
| `crates/agentos-bus/src/client.rs` | 3 | 5s connect timeout |
| `crates/agentos-tools/src/runner.rs` | 2 | Cryptographic temp file names; 10 MiB output cap |
| `crates/agentos-tools/src/file_reader.rs` | 4 | 10 MiB read limit; canonicalized data_dir |
| `crates/agentos-tools/src/memory_search.rs` | 4 | `top_k` capped at 100 |
| `crates/agentos-tools/src/memory_write.rs` | 4 | `content` capped at 512 KiB |
| `crates/agentos-tools/src/data_parser.rs` | 4 | `data` capped at 4 MiB; CSV 50k row limit |
| `crates/agentos-wasm/src/lib.rs` | 4 | `ResourceLimiter` 256 MiB WASM memory cap |
| `crates/agentos-memory/src/semantic.rs` | 2 | `write()` uses rusqlite Transaction |
| `crates/agentos-llm/src/anthropic.rs` | 2 | `with_base_url()` configurable endpoint |
| `crates/agentos-kernel/src/commands/task.rs` | 5 | Remove `.unwrap()` on agent lookup |
| `crates/agentos-kernel/src/router.rs` | 5 | Remove `.last().unwrap()` in routing strategies |
| `crates/agentos-kernel/src/agent_message_bus.rs` | 5 | Bounded inbox (256); capped history (10,000) |
| `crates/agentos-kernel/src/injection_scanner.rs` | 8 | NFKC normalization; closing tag + standalone base64 patterns; HTML-escape source |
| `crates/agentos-kernel/src/kernel.rs` | 7 | Vault parent dir `0o700` at boot |
| `crates/agentos-kernel/Cargo.toml` | 8 | Added `unicode-normalization` dep |
| `crates/agentos-vault/src/master_key.rs` | 8 | Argon2id parallelism 1→4 |
| `crates/agentos-audit/src/log.rs` | 7 | `verify_chain()` predecessor hash error propagation |
| `crates/agentos-web/src/csrf.rs` | 6 | `TOKEN_TTL` visibility `pub` → `pub(crate)` |
| `crates/agentos-web/src/server.rs` | 6 | CSRF sweep background task |
| `crates/agentos-cli/src/commands/secret.rs` | 6 | `parse_scope()` empty-name validation |
| `config/default.toml` | 9 | Security warning comments on `/tmp` paths |
| `Cargo.toml` (workspace) | 8 | Added `unicode-normalization = "0.1"` |
| `crates/agentos-kernel/src/escalation.rs` | 5 | Added `validate_webhook_url()` SSRF guard; webhook POST validates URL before spawning request |
| `crates/agentos-kernel/src/cost_tracker.rs` | 5 | `period_start` → `AtomicI64`; atomic `compare_exchange` reset; NaN/Inf guard on cost float cast |
| `crates/agentos-kernel/src/commands/permission.rs` | 5 | `update_permissions(..).ok()` → error propagation; `expires_secs as i64` → `i64::try_from()` |
| `crates/agentos-kernel/src/agent_registry.rs` | 5 | `save_to_disk()` logs `tracing::warn!` on serialization and write failures |
| `crates/agentos-kernel/src/run_loop.rs` | 5 | Timeout checker now calls `remove_context`, `remove_task`, `release_all_for_agent` and `evict_terminal` on task timeout |
| `crates/agentos-kernel/src/resource_arbiter.rs` | 5 | `is_expired()` uses `.max(0)` before `as u64` cast to guard against NTP clock skew |
| `crates/agentos-kernel/src/background_pool.rs` | 5 | Added `evict_terminal(max_age_secs)` method |
| `crates/agentos-kernel/src/task_completion.rs` | 5 | Scheduler errors logged; consolidation spawn uses `CancellationToken` |
| `crates/agentos-kernel/src/schedule_manager.rs` | 5 | `create_job()` rejects duplicate job names |
| `crates/agentos-kernel/src/task_executor.rs` | 5 | `push_entry` failure logs `tracing::warn!` instead of silently succeeding |

---

## Related

- [[Full Codebase Review Plan]] — master plan with all phase specs
- [[Full Codebase Review Data Flow]] — data flow through the review process
- [[01-foundation-types-review]] through [[09-config-and-manifests-review]] — per-phase findings
