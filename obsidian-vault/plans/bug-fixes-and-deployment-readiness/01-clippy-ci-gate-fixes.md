---
title: Fix Clippy CI Gate Errors
tags:
  - kernel
  - v3
  - bugfix
date: 2026-03-13
status: complete
effort: 1h
priority: critical
---

# Fix Clippy CI Gate Errors

> Fix the 4 clippy errors that prevent `cargo clippy --workspace -- -D warnings` from passing, unblocking CI.

---

## Why This Phase

CI enforces `cargo clippy --workspace -- -D warnings`. As of 2026-03-13, there are exactly 4 clippy errors in `agentos-kernel`, all with mechanical fixes. Until these are fixed, no other code can merge through CI. This phase has no dependencies and should be done first.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `commands/escalation.rs:214` | `if task_resumed { Info } else if approved && !blocking { Info }` -- identical branches | Collapse to single condition: `if task_resumed \|\| (approved && !blocking) { Info } else { Warn }` |
| `event_bus.rs:531` | Nested `if value.len() >= 2 { if (value.starts_with(...)) }` | Collapse into single `if value.len() >= 2 && (...)` |
| `event_dispatch.rs:39` | `trace_id.unwrap_or_else(TraceID::new)` | `trace_id.unwrap_or_default()` |
| `memory_extraction.rs:193` | `ExtractionRegistry::new()` has no `Default` impl | Add `impl Default for ExtractionRegistry` delegating to `Self::new()` |

---

## What to Do

### 1. Fix `if_same_then_else` in `commands/escalation.rs`

Open `crates/agentos-kernel/src/commands/escalation.rs`, line ~214.

Current code:
```rust
severity: if task_resumed {
    agentos_audit::AuditSeverity::Info
} else if approved && !blocking {
    agentos_audit::AuditSeverity::Info
} else {
    agentos_audit::AuditSeverity::Warn
},
```

Replace with:
```rust
severity: if task_resumed || (approved && !blocking) {
    agentos_audit::AuditSeverity::Info
} else {
    agentos_audit::AuditSeverity::Warn
},
```

### 2. Fix `collapsible_if` in `event_bus.rs`

Open `crates/agentos-kernel/src/event_bus.rs`, line ~531.

Current code:
```rust
if value.len() >= 2 {
    if (value.starts_with('\'') && value.ends_with('\''))
        || (value.starts_with('"') && value.ends_with('"'))
    {
        return Some(value[1..value.len() - 1].to_string());
    }
}
```

Replace with:
```rust
if value.len() >= 2
    && ((value.starts_with('\'') && value.ends_with('\''))
        || (value.starts_with('"') && value.ends_with('"')))
{
    return Some(value[1..value.len() - 1].to_string());
}
```

### 3. Fix `unwrap_or_default` in `event_dispatch.rs`

Open `crates/agentos-kernel/src/event_dispatch.rs`, line ~39.

Change:
```rust
let trace_id = trace_id.unwrap_or_else(TraceID::new);
```
To:
```rust
let trace_id = trace_id.unwrap_or_default();
```

This works because `TraceID` implements `Default` (it wraps `Uuid::new_v4()`).

### 4. Add `Default` impl for `ExtractionRegistry` in `memory_extraction.rs`

Open `crates/agentos-kernel/src/memory_extraction.rs`, near line ~192.

Add before the `impl ExtractionRegistry` block:
```rust
impl Default for ExtractionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/commands/escalation.rs` | Collapse identical `if` branches (line ~214) |
| `crates/agentos-kernel/src/event_bus.rs` | Collapse nested `if` (line ~531) |
| `crates/agentos-kernel/src/event_dispatch.rs` | Use `unwrap_or_default()` (line ~39) |
| `crates/agentos-kernel/src/memory_extraction.rs` | Add `Default` impl for `ExtractionRegistry` |

---

## Prerequisites

None -- this is the first phase and has no dependencies.

---

## Test Plan

- All existing tests must continue to pass: `cargo test -p agentos-kernel`
- The specific logic paths in `cmd_resolve_escalation` are covered by the existing escalation manager tests (the audit severity value does not affect test assertions, only the log entry)
- `ExtractionRegistry::default()` should produce the same result as `ExtractionRegistry::new()` -- verify by checking that both return empty registries

---

## Verification

```bash
cargo clippy --workspace -- -D warnings
cargo test -p agentos-kernel
cargo fmt --all -- --check
```

All three commands must exit with code 0.
