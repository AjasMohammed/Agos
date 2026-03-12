---
title: Critical Build Fix — AuditEntry Missing Fields
tags:
  - next-steps
  - bug
  - critical
date: 2026-03-11
status: done
effort: 30min
priority: blocker
---

# Critical Build Fix — AuditEntry Missing Fields

> [!danger] Blocker
> The workspace does not compile. Fix this before any other task.

---

## What Broke

When `reversible: bool` and `rollback_ref: Option<String>` were added to `AuditEntry` in `crates/agentos-audit/src/log.rs`, the **test functions in the same file** still construct `AuditEntry` with all fields listed explicitly. Rust's struct literal syntax requires all fields — so these 6 usages fail with `E0063: missing fields`.

**Compiler errors (all in `crates/agentos-audit/src/log.rs`):**

```
error[E0063]: missing fields `reversible` and `rollback_ref` in initializer of `log::AuditEntry`
  --> crates/agentos-audit/src/log.rs:590:24
  --> crates/agentos-audit/src/log.rs:625:24
  --> crates/agentos-audit/src/log.rs:659:24
  (+ 3 more)
```

---

## The Fix

In `crates/agentos-audit/src/log.rs`, find every `AuditEntry { ... }` struct literal that is missing the two new fields and add:

```rust
reversible: false,
rollback_ref: None,
```

These defaults are correct for test-only entries — they represent non-reversible audit events with no snapshot reference.

### Exact Search Pattern

Search for `AuditEntry {` in `log.rs`, then check if each instance has `reversible:` and `rollback_ref:`. The 6 failing instances are in the test module (`#[cfg(test)]`).

### Quick Patch Command

```bash
# Verify the 6 locations
grep -n "AuditEntry {" crates/agentos-audit/src/log.rs

# After identifying each closing `};` for a test AuditEntry,
# insert the two fields before the closing brace.
```

### Struct Definition Reference (for context)

```rust
// crates/agentos-audit/src/log.rs
pub struct AuditEntry {
    pub seq: u64,
    pub prev_hash: String,
    pub timestamp: DateTime<Utc>,
    pub agent_id: AgentID,
    pub task_id: Option<TaskID>,
    pub event_type: AuditEventType,
    pub severity: AuditSeverity,
    pub detail: serde_json::Value,
    pub entry_hash: String,
    /// Whether the action that produced this entry is reversible via rollback.
    pub reversible: bool,                   // ← NEW field
    pub rollback_ref: Option<String>,       // ← NEW field
}
```

---

## Verification

After applying the fix:

```bash
cargo test -p agentos-audit
# Should output: test result: ok. N passed; 0 failed

cargo build --workspace
# Should compile cleanly
```

---

## Related

- [[03-Snapshot Rollback]] — The `reversible` and `rollback_ref` fields are the foundation for the full checkpoint/rollback system
- [[Index]] — Back to dashboard