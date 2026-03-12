---
title: "Phase 3: Bus & Capability Review"
tags:
  - review
  - security
  - bus
  - capability
  - phase-3
date: 2026-03-13
status: planned
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
| `crates/agentos-capability/src/lib.rs` | 9 | Re-exports |
| `crates/agentos-capability/src/token.rs` | 37 | `CapabilityToken` struct |
| `crates/agentos-capability/src/permissions.rs` | 54 | `PermissionSet` types |
| `crates/agentos-capability/src/profiles.rs` | 105 | Permission profiles for roles |
| `crates/agentos-capability/src/engine.rs` | 453 | Token validation, HMAC, permission enforcement |

**Checklist:**
- [ ] HMAC-SHA256 signing covers all security-relevant fields (agent_id, permissions, expiry — not just a subset)
- [ ] Token expiry checked on **every** validation, not just at creation
- [ ] HMAC comparison is constant-time (prevent timing attacks)
- [ ] Permission escalation: a token cannot grant more permissions than its issuer had
- [ ] Profile definitions match documented permission sets
- [ ] Token revocation: is there a mechanism? If not, is expiry short enough?
- [ ] Inline tests cover: expired token, wrong HMAC, missing permissions, escalation attempt

---

## Step 3.2 — Bus: Messages & Transport (~812 lines)

**Files to read:**

| File | Lines | What It Contains |
|------|-------|-----------------|
| `crates/agentos-bus/src/lib.rs` | 104 | Re-exports |
| `crates/agentos-bus/src/message.rs` | 378 | `KernelCommand` enum (26+ variants), `BusMessage` |
| `crates/agentos-bus/src/transport.rs` | 56 | Wire protocol utilities |
| `crates/agentos-bus/src/client.rs` | 48 | IPC client |
| `crates/agentos-bus/src/server.rs` | 65 | IPC server |
| `crates/agentos-bus/src/tls_server.rs` | 161 | TLS support |

**Checklist:**
- [ ] `KernelCommand` enum covers all 26+ command variants with correct payloads
- [ ] Message serialization/deserialization is robust (handles malformed input)
- [ ] Unix socket permissions: socket file has restricted permissions (0600 or 0660)
- [ ] TLS server validates certificates properly
- [ ] Message size limits to prevent DoS via oversized messages
- [ ] Client connection timeout and retry logic
- [ ] No deserialization of untrusted data into code execution paths

---

## Files Changed

No files changed — read-only review phase.

## Dependencies

Phase 1 (types understood).

## Verification

```bash
cargo build -p agentos-capability -p agentos-bus
cargo test -p agentos-capability -p agentos-bus
```

---

## Related

- [[Full Codebase Review Plan]]
- [[08-security-deep-dives]] — Step 8.1 does an adversarial deep dive on capability tokens
