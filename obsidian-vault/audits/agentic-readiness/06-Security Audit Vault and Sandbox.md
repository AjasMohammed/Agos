---
title: "Audit #6: Security, Audit, Vault & Sandbox"
tags:
  - audit
  - security
  - vault
  - sandbox
  - capability
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 2h
priority: critical
---

# Audit #6: Security, Audit, Vault & Sandbox

> Evaluating the security infrastructure from an agent's perspective — can I trust this OS to protect me and constrain me appropriately?

---

## Scope

- `crates/agentos-capability/` — HMAC-SHA256 token signing, permission engine
- `crates/agentos-audit/` — Append-only SQLite audit log with SHA256 hash chain
- `crates/agentos-vault/` — AES-256-GCM encrypted secrets, Argon2id key derivation
- `crates/agentos-sandbox/` — Seccomp-BPF + resource limits for tool isolation

---

## Verdict: STRONG — best-in-class for an agent OS; minor gaps in operational details

Security is the strongest pillar of AgentOS. AES-256-GCM encryption, HMAC-SHA256 token signing, Ed25519 manifest verification, seccomp-BPF sandboxing, and comprehensive audit logging. The zero-exposure proxy token system for secrets is particularly well-designed. Some operational gaps exist.

---

## Findings

### 1. Capability Engine — EXCELLENT

**Architecture:**
- HMAC-SHA256 signing of all token fields (task_id, agent_id, tools, intents, permissions + deny entries, expiry).
- Constant-time verification via `hmac::verify_slice()`.
- Key persisted encrypted in vault.

**What works well for me as an agent:**
- My capability token is unforgeable — the kernel signs it, I can't modify it.
- Permission checks include deny entries in the signature — can't be stripped.
- Expiry is double-checked at validation time.
- Cross-engine tokens are rejected (different HMAC keys).

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 1 | **Redundant permission expiry check** — `check()` and then explicit `expires_at` iteration | Low | Correct but inefficient |
| 2 | **Lock poisoning recovery silently continues** — uses `unwrap_or_else(|e| e.into_inner())` | Medium | Could operate on stale data after panic |

### 2. Audit Log — SOLID

**Architecture:**
- Append-only SQLite with 80+ event types.
- SHA256 hash chain for tamper detection.
- Severity levels + reversibility flags.
- Per-event metadata with structured payloads.

**What works well for me as an agent:**
- Every tool call, permission check, and security event is recorded.
- I can query my own audit trail for debugging.
- Hash chain means post-hoc tampering is detectable.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 3 | **Hash chain not verified on read or startup** — no `verify_chain()` method | Medium | Tampering goes undetected until manual audit |
| 4 | **Reversible flag not enforced** — `reversible: false` is metadata only, no rollback prevention | Medium | Non-reversible actions could theoretically be rolled back |
| 5 | **Audit write failures are silent** — task completion succeeds even if audit fails | Medium | Lost audit trail under high write load |

### 3. Vault — EXCELLENT

**Architecture:**
- AES-256-GCM encryption (AEAD standard).
- Argon2id key derivation for master password.
- 12-byte random nonce per encryption (correct for AES-GCM).
- Secret scopes: Kernel, Global, Agent(id), Tool(id).
- Proxy token system: one-time use, 5-second TTL, auto-expiring.
- Emergency lockdown: revokes all proxy tokens atomically.
- File permissions: 0o600 on vault DB.

**What works well for me as an agent:**
- I never see raw secrets — only proxy tokens with 5-second TTL.
- Kernel secrets are protected from agent rotation/revocation.
- Scope enforcement: I can only access secrets in my scope.
- `secret_headers` in http-client inject secrets without exposing them to me.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 6 | **Proxy token error message leaks validity** — "Unknown or already-used" distinguishable | Medium | Timing-based token enumeration possible |
| 7 | **Rotated secrets don't invalidate old proxy tokens** — race window during rotation | High | Agent could use stale secret between rotation and expiry |
| 8 | **Decryption errors not distinguishable** — wrong key vs corrupted data vs invalid tag all same error | Low | Debugging difficulty |

### 4. Sandbox — EXCELLENT

**Architecture:**
- Seccomp-BPF syscall filtering (Linux-only).
- Resource limits: RLIMIT_AS (memory), RLIMIT_CPU (time), RLIMIT_NPROC (forks=4), RLIMIT_FSIZE (disk), RLIMIT_NOFILE (256 FDs).
- File descriptor closing: close_range() with fallback.
- Environment sanitization: only PATH, HOME, LANG.
- No-new-privs flag prevents privilege escalation.
- Stdout/stderr capped at 10 MiB.

**What works well for me as an agent:**
- Tool execution is properly isolated — even a malicious tool can't escape.
- Resource limits prevent accidental DoS.
- Environment sanitization prevents secret leakage.
- FD closing prevents access to parent's network connections.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 9 | **FD close fallback only to 1024** — FDs > 1024 not closed on older kernels | Medium | Potential information leak from parent FDs |
| 10 | **Pre-exec hook errors not propagated** — setrlimit failure causes child exit, parent misattributes error | Medium | Confusing error messages for sandbox failures |
| 11 | **Temp files not always cleaned up** — `.ok()` on remove silently ignores errors | Low | Disk space leak over time |

---

## Security Posture Summary

| Layer | Rating | Key Strength | Key Gap |
|-------|--------|-------------|---------|
| Authentication (Capability) | 4.5/5 | HMAC-SHA256, constant-time verify | Lock poisoning recovery |
| Authorization (Permissions) | 4.5/5 | Deny-first, SSRF, path-prefix, expiry | No wildcard support |
| Encryption (Vault) | 4.5/5 | AES-256-GCM, Argon2id, proxy tokens | Rotation doesn't invalidate proxies |
| Isolation (Sandbox) | 4.5/5 | Seccomp-BPF, rlimits, env sanitization | FD close fallback limit |
| Audit (Log) | 4.0/5 | Append-only, hash chain, 80+ event types | Chain not verified at startup |
| Injection Defense | 4.0/5 | NFKC normalization, 25+ patterns | High false-positive rate |
| **Overall Security** | **4.3/5** | Comprehensive multi-layer defense | Operational gaps in rotation/verification |

---

## Score

| Criterion | Score (1-5) | Notes |
|-----------|------------|-------|
| Cryptographic Correctness | 4.5 | AES-256-GCM, HMAC-SHA256, Ed25519 all properly implemented |
| Permission Model | 4.5 | Deny-first, SSRF, path-prefix, expiry — comprehensive |
| Secrets Management | 4.0 | Proxy tokens excellent, but rotation gap |
| Sandbox Isolation | 4.5 | Seccomp-BPF, rlimits, FD closing — thorough |
| Audit Trail | 4.0 | Hash chain but no verification; silent write failures |
| **Overall** | **4.3/5** | Strongest subsystem in AgentOS |
