---
title: Spec Enforcement Hardening — Remaining Gaps
tags:
  - kernel
  - security
  - phase-3
  - next-steps
  - feature
date: 2026-03-11
status: complete
effort: 4h
priority: high
---

# Spec Enforcement Hardening — Remaining Gaps

> Six enforcement-level gaps where the machinery exists but isn't fully wired per the spec.

---

## Current State

A detailed code review against `agos-implementation-spec.md` confirmed all 12 spec items have foundational implementations. However, six enforcement-level gaps remain:

| # | Spec | Gap | Impact |
|---|------|-----|--------|
| 1 | #12 | Escalation has no `expires_at` / auto-deny | Pending escalations hang forever |
| 2 | #5  | Snapshots never expire | Disk usage grows unbounded |
| 3 | #2  | PermissionSet uses exact string match | Path `/home/user/docs/x.txt` doesn't match grant on `fs:/home/user/` |
| 4 | #6  | System prompt missing injection standing instruction | LLM doesn't know to distrust `<user_data>` tags |
| 5 | #4  | No structured cost attribution audit entries | Cost forensics limited |
| 6 | #8  | `sweep_expired()` not called periodically | TTL locks can linger |

---

## Goal / Target State

1. Escalations auto-deny after a configurable timeout (default 5 minutes)
2. Snapshots older than 72h are automatically cleaned up
3. Permission checks support path prefix matching and deny-list entries
4. System prompt includes standing injection safety instruction
5. Each inference writes a structured cost attribution entry to the audit log
6. Resource arbiter expired locks are swept every 10 seconds

---

## Step-by-Step Plan

1. **Escalation auto-expiry** — Add `expires_at` to `PendingEscalation`, add `sweep_expired()` method, wire into agentd loop
2. **Snapshot expiration** — Add `sweep_expired_snapshots()` to Kernel, wire into timeout checker loop
3. **Permission hierarchy** — Enhance `PermissionSet.check()` with prefix matching + add deny entries
4. **Injection safety instruction** — Append standing instruction to system prompt in `task_executor.rs`
5. **Cost attribution audit** — After `record_inference()`, write structured cost JSON to audit log
6. **Arbiter sweep wiring** — Call `resource_arbiter.sweep_expired()` in the timeout checker loop

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/escalation.rs` | Add `expires_at`, `sweep_expired()`, default timeout |
| `crates/agentos-kernel/src/snapshot.rs` | Add `sweep_expired_snapshots()` |
| `crates/agentos-kernel/src/run_loop.rs` | Wire escalation sweep + snapshot sweep + arbiter sweep |
| `crates/agentos-types/src/capability.rs` | Add prefix matching + deny entries to `PermissionSet` |
| `crates/agentos-kernel/src/task_executor.rs` | Injection instruction + cost audit entry |

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Related

[[agos-implementation-spec]]
[[10-High-Priority Gaps]]
