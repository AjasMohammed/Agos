---
title: "Phase 3: Bus & Capability Review"
tags:
  - review
  - security
  - bus
  - capability
  - phase-3
date: 2026-03-13
status: complete
effort: 1h
priority: critical
---

# Phase 3: Bus & Capability Review

> Review the capability token engine (authorization boundary) and the IPC bus (CLI ↔ kernel transport).

---

## Why This Phase

The capability engine is the **authorization boundary for the entire system** — every tool execution validates against it. A flaw here means privilege escalation. The bus is the transport layer — a deserialization bug or missing size limit means DoS or code execution.

---

## Current → Target State

- **Current:** `agentos-capability` (5 files, 658 lines), `agentos-bus` (6 files, 812 lines) — no dedicated test files
- **Target:** Token lifecycle validated for forgery resistance, expiry enforcement, constant-time comparison; bus validated for message safety

---

## Step 3.1 — Capability Engine (~658 lines) `SECURITY-CRITICAL`

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-capability/src/lib.rs` | 10 | Re-exports |
| `crates/agentos-capability/src/token.rs` | 80 | HMAC signing + verification (single canonical field layout) |
| `crates/agentos-capability/src/permissions.rs` | 54 | `PermissionSet` types |
| `crates/agentos-capability/src/profiles.rs` | 105 | Permission profiles for roles |
| `crates/agentos-capability/src/engine.rs` | 510 | Token validation, permission enforcement, 9 tests |

**Checklist:**
- [x] HMAC-SHA256 signing covers all security-relevant fields (agent_id, permissions, expiry — not just a subset)
  - **FIXED:** `deny_entries` and `PermissionEntry.expires_at` were NOT signed — added to `feed_token_fields()` with length-prefixed encoding
  - **FIXED:** Added length-prefixed encoding for all variable-length fields to prevent concatenation collision attacks
- [x] Token expiry checked on **every** validation, not just at creation
  - Verified: `validate_intent` checks `chrono::Utc::now() > token.expires_at` at line 193, no bypass path
- [x] HMAC comparison is constant-time (prevent timing attacks)
  - Verified: `verify_token_signature` uses `mac.verify_slice()` from the hmac crate (constant-time internally)
  - **FIXED:** Deduplicated HMAC logic — `verify_signature()` now delegates to `verify_token_signature()` in token.rs, eliminating divergence risk
- [ ] Permission escalation: a token cannot grant more permissions than its issuer had
  - **DEFERRED:** `issue_token` trusts its caller. Kernel call sites use `compute_effective_permissions()` + `intersect()` correctly, but no defense-in-depth guard inside the engine. See [[08-security-deep-dives]] for follow-up.
- [x] Profile definitions match documented permission sets
  - Verified: `ProfileManager` stores/retrieves correctly. Note: profiles are not yet wired into token issuance (dead feature).
- [x] Token revocation: is there a mechanism? If not, is expiry short enough?
  - No revocation list. Default TTLs are 60-300s, limiting exposure. `revoke_agent()` removes permissions but does not invalidate issued tokens. Acceptable with short TTLs.
- [x] Inline tests cover: expired token, wrong HMAC, missing permissions, escalation attempt
  - **ADDED:** `test_deny_entries_tampering_invalidates_signature`, `test_expires_at_tampering_invalidates_signature`, `test_cross_engine_token_rejected`, `test_serialization_roundtrip_preserves_signature`
- [x] **ADDED:** Signing key zeroized on drop via `zeroize` crate (per project convention)

---

## Step 3.2 — Bus: Messages & Transport (~812 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-bus/src/lib.rs` | 104 | Re-exports + integration tests |
| `crates/agentos-bus/src/message.rs` | 414 | `KernelCommand` enum (90+ variants), `BusMessage` |
| `crates/agentos-bus/src/transport.rs` | 80 | Wire protocol with size limits + timeouts |
| `crates/agentos-bus/src/client.rs` | 55 | IPC client with connect timeout |
| `crates/agentos-bus/src/server.rs` | 78 | IPC server with 0600 socket permissions |
| `crates/agentos-bus/src/tls_server.rs` | 161 | TLS support |

**Checklist:**
- [x] `KernelCommand` enum covers all command variants with correct payloads
  - Verified: 90+ variants across agent, task, tool, secret, permission, role, schedule, background, escalation, cost, pipeline, resource, snapshot, HAL, event management
- [x] Message serialization/deserialization is robust (handles malformed input)
  - **FIXED:** Added `len == 0` rejection in `read_message`
  - Verified: serde_json errors are caught and returned as `Serialization` errors
- [x] Unix socket permissions: socket file has restricted permissions (0600 or 0660)
  - **FIXED:** Added `std::fs::set_permissions(socket_path, 0o600)` after `UnixListener::bind`
- [x] TLS server validates certificates properly
  - Verified: server-side cert is loaded correctly. Note: `with_no_client_auth()` means no mTLS — acceptable for initial deployment but should be hardened for production remote access
- [x] Message size limits to prevent DoS via oversized messages
  - **FIXED:** Added `MAX_MESSAGE_SIZE` constant (16 MiB) shared between read and write paths; write path now validates size before `u32` cast (prevents silent truncation)
- [x] Client connection timeout and retry logic
  - **FIXED:** Added 5-second connect timeout to `BusClient::connect`; added 30-second I/O timeout to `read_message` (prevents slowloris-style attacks)
- [x] No deserialization of untrusted data into code execution paths
  - Verified: all deserialization targets typed enums via serde; no `unsafe` or dynamic dispatch from deserialized data

---

## Remaining Issues (deferred to future phases)

| Severity | Issue | Deferred To |
|----------|-------|-------------|
| HIGH | TLS server lacks mTLS (client auth) | [[08-security-deep-dives]] |
| HIGH | `SetSecret`/`RotateSecret` use `String` not `ZeroizingString` | [[08-security-deep-dives]] |
| MEDIUM | No escalation guard inside `issue_token` | [[08-security-deep-dives]] Step 8.1 |
| MEDIUM | No token revocation mechanism (relies on short TTL) | [[08-security-deep-dives]] |
| MEDIUM | No concurrent connection limit (unbounded `tokio::spawn`) | Bus hardening phase |
| MEDIUM | Rate limiter HashMap can grow unboundedly with fake agent names | Bus hardening phase |
| MEDIUM | Stale socket removal has TOCTOU race | Bus hardening phase |
| LOW | `ProfileManager` not connected to token issuance (dead feature) | Feature backlog |
| LOW | Per-connection rate limit (50/sec) is hard-coded, not configurable | Config phase |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-capability/src/token.rs` | Rewrote HMAC signing: `feed_token_fields()` + `compute_signature()` + `verify_token_signature()` with length-prefixed encoding, deny_entries, expires_at |
| `crates/agentos-capability/src/engine.rs` | `verify_signature()` delegates to `verify_token_signature()`; added `Drop` impl for key zeroization; added 4 security tests |
| `crates/agentos-capability/src/lib.rs` | Re-export `verify_token_signature` |
| `crates/agentos-capability/Cargo.toml` | Added `zeroize` dependency |
| `crates/agentos-bus/src/transport.rs` | Added `MAX_MESSAGE_SIZE` constant, write-side size validation, `try_into` for u32 cast, 30s I/O timeouts, `len == 0` rejection |
| `crates/agentos-bus/src/server.rs` | Added `0o600` socket permissions after bind |
| `crates/agentos-bus/src/client.rs` | Added 5-second connect timeout |

## Dependencies

Phase 1 (types understood).

## Verification

```bash
cargo build -p agentos-capability -p agentos-bus   # ✅ PASSED
cargo test -p agentos-capability -p agentos-bus     # ✅ 11 tests passed (9 capability + 2 bus)
cargo build --workspace                              # ✅ PASSED (no downstream breakage)
cargo test --workspace                               # ✅ 41 test suites, 0 failures
```

---

## Related

- [[Full Codebase Review Plan]]
- [[08-security-deep-dives]] — Step 8.1 does an adversarial deep dive on capability tokens
