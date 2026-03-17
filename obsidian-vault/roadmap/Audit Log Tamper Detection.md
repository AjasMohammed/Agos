---
title: Audit Log Tamper Detection
tags: [roadmap, security, audit]
date: 2026-03-17
status: deferred
priority: medium
---

# Audit Log Tamper Detection

> Formally deferred: no tamper detection infrastructure exists to back the `AuditLogTamperAttempt` event type; implementation requires a dedicated audit integrity subsystem.

---

## Problem

The event type `AuditLogTamperAttempt` is defined in `crates/agentos-types/src/event.rs` and is referenced in trigger prompt handling. However, it is **never emitted** anywhere in the codebase because no tamper detection infrastructure exists.

Without a mechanism to detect when audit log rows have been modified, deleted, or reordered after the fact, the `AuditLogTamperAttempt` event is a dead type — its presence in the type system implies a security guarantee that is not actually enforced.

## What Would Be Required

Two viable implementation approaches:

### Option A — Row-level checksums

- Add a `row_hash TEXT` column to the audit log SQLite table.
- On each insert, compute a hash (e.g. SHA-256) over the row's canonical fields and store it alongside the row.
- Alternatively, use a SQLite `AFTER INSERT` trigger to write the hash automatically.
- A periodic or on-demand integrity scan re-computes hashes and compares; any mismatch triggers `AuditLogTamperAttempt`.

**Trade-offs:** Simple to implement; does not detect row deletion or reordering; hash is stored in the same DB that could be tampered with.

### Option B — Merkle chain over sequential row IDs

- Each audit row stores `prev_hash TEXT` — the hash of the previous row's content + its own `prev_hash`.
- This creates a hash chain: tampering with any row invalidates all subsequent rows.
- A verification pass walks the chain from genesis and checks each link.
- Emits `AuditLogTamperAttempt` on any broken link.

**Trade-offs:** Stronger integrity guarantee; detects deletion and reordering; more complex to implement; verification is O(n) over the full log.

## Formal Deferral

This item is **deferred**, not abandoned. Reasons for deferral:

1. No attacker has write access to the SQLite audit DB in the current threat model (local, single-user deployment).
2. Implementing either option correctly requires careful schema migration and backward-compatibility handling for existing audit databases.
3. The effort is non-trivial and should be planned as a dedicated **Audit Integrity Subsystem** with its own design doc, not bolted onto the existing `agentos-audit` crate without prior planning.

This is filed as a **roadmap item**, not a bug. The `AuditLogTamperAttempt` event type should remain defined so that the type system is ready when the subsystem is built.

## When to Revisit

- When AgentOS moves toward multi-user or networked deployments where audit log integrity has a stronger attacker model.
- When a formal security audit recommends log integrity as a requirement.
- After the core kernel, memory, and event trigger systems reach stability.

## Related

- [[Issues and Fixes]]
- [[V3 Roadmap]]
- `crates/agentos-audit/src/log.rs`
- `crates/agentos-types/src/event.rs`
