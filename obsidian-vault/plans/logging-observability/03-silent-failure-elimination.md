---
title: Phase 3 — Silent Failure Elimination
tags:
  - observability
  - logging
  - kernel
  - phase-3
  - next-steps
date: 2026-03-21
status: planned
effort: 1d
priority: high
---

# Phase 3 — Silent Failure Elimination

> Convert all deliberate `.ok()` suppression and `let _ =` error discards into logged warnings, so no error in the system is invisible. Preserves existing non-fatal semantics while making every failure observable.

---

## Why This Phase

There are 27+ instances of `.ok()` in `task_executor.rs` alone, plus scattered `let _ =` patterns across the codebase. These are intentional (failures are non-fatal), but they are completely invisible. In production, if the scheduler stops accepting requeueing (e.g., channel closed), tasks will silently timeout with no indication of why.

This phase does NOT change error handling semantics — errors that were non-fatal remain non-fatal. The only change is: failures become visible in the log.

---

## Current → Target State

**Before:**
```rust
kernel.scheduler.requeue(&waiter_id).await.ok();
```
→ If this fails, nothing. The task times out with no log.

**After:**
```rust
if let Err(e) = kernel.scheduler.requeue(&waiter_id).await {
    tracing::warn!(
        error = %e,
        waiter_id = %waiter_id,
        "Requeue failed — waiter will timeout naturally"
    );
}
```
→ Failure visible; semantics unchanged (we still don't abort the task).

---

## Inventory of Silent Failures

### `crates/agentos-kernel/src/task_executor.rs` — 27 instances

These are all of the form `kernel.scheduler.requeue(&id).await.ok()`. They appear at:
- Multiple points in the waiter notification loop
- Task completion signal sends
- Context write-back operations

**Pattern to apply to ALL 27:**
```rust
// Before
some_operation.await.ok();

// After
if let Err(e) = some_operation.await {
    tracing::warn!(error = %e, context = "description of what this was doing", "Non-fatal operation failed");
}
```

**How to find all 27:** Search for `.ok();` in `task_executor.rs` (note the semicolon — these are discarded results, not `.ok()?` which propagate):
```bash
grep -n '\.ok();' crates/agentos-kernel/src/task_executor.rs
```

---

### `crates/agentos-kernel/src/run_loop.rs` — 3 instances (lines ~197, ~277, ~329)

Same requeue pattern. Apply same fix.

---

### `crates/agentos-kernel/src/scheduler.rs` — review `let _ =` patterns

Search for `let _ =` in scheduler:
```bash
grep -n 'let _ =' crates/agentos-kernel/src/scheduler.rs
```

For each instance, classify:
- If it's a channel send where the receiver may have dropped: `warn!` on error with the field name
- If it's a metric increment: acceptable to keep silent (metrics failures are truly non-critical)
- If it's anything else: must have a `warn!`

---

### `crates/agentos-kernel/src/commands/*.rs` — review handler modules

Each command handler (agent.rs, background.rs, pipeline.rs, task.rs) may have silent discards:
```bash
grep -rn '\.ok();' crates/agentos-kernel/src/commands/
grep -rn 'let _ =' crates/agentos-kernel/src/commands/
```

Triage each by the same rule: non-metric `let _ =` or `.ok();` needs a `warn!`.

---

### `crates/agentos-bus/src/lib.rs` — IPC send failures

Message send failures over the Unix socket should be `warn!` not silent:
```bash
grep -n '\.ok();' crates/agentos-bus/src/lib.rs
```

---

### `crates/agentos-tools/src/runner.rs` — stderr discard

If shell stderr output is being discarded silently on process spawn failure, add a warn.

---

## Classification Rules

| Pattern | Action |
|---------|--------|
| `metrics_counter.increment().ok()` | Keep silent — metrics are always best-effort |
| `channel.send(x).ok()` / `channel.send(x).await.ok()` | Add `warn!` — indicates receiver dropped, which is unexpected |
| `scheduler.requeue(...).await.ok()` | Add `warn!` — indicates scheduler degradation |
| `audit.append(...).ok()` | **ERROR level** — audit failures are serious; already handled in event_dispatch.rs |
| `let _ = tokio::spawn(...)` | Check if spawn failure is possible; add `warn!` if it can fail |
| `let _ = tx.send(...)` | Add `warn!` — indicates channel closed unexpectedly |

---

## Noise Management

Some requeue failures may be spurious during normal shutdown (channels closing). To prevent log spam during shutdown:

```rust
// Check if kernel is shutting down before logging
if !kernel.is_shutting_down() {
    tracing::warn!(error = %e, "Requeue failed");
}
```

Or use a rate-limited warn if the operation is in a tight loop:
```rust
// Only warn once per 100 failures to avoid log flooding
if failure_count % 100 == 1 {
    tracing::warn!(error = %e, failure_count, "Requeue failing repeatedly");
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Convert 27 `.ok()` calls to `if let Err` + warn |
| `crates/agentos-kernel/src/run_loop.rs` | Convert 3 `.ok()` calls to `if let Err` + warn; add restart warn |
| `crates/agentos-kernel/src/scheduler.rs` | Triage and log `let _ =` patterns |
| `crates/agentos-kernel/src/commands/agent.rs` | Triage `.ok()` discards |
| `crates/agentos-kernel/src/commands/task.rs` | Triage `.ok()` discards |
| `crates/agentos-bus/src/lib.rs` | Log send failures |

---

## Dependencies

- [[01-span-instrumentation]] should be complete first so the new `warn!` lines inherit span context (task_id, agent_id). Not a hard blocker, but the warnings will be context-free without it.

---

## Test Plan

1. `cargo build -p agentos-kernel -p agentos-bus` — must compile clean
2. Grep for remaining `.ok();` in kernel crate — should be zero (excluding metrics calls)
3. `cargo test --workspace` — all pass (no semantics changed, only logging added)
4. Trigger a forced requeue failure in tests (mock scheduler returning Err) — confirm warn appears in test log output

---

## Verification

```bash
cargo build --workspace
cargo test --workspace

# Count remaining silent .ok() calls in kernel (should be 0 for non-metrics)
grep -rn '\.ok();' crates/agentos-kernel/src/ | grep -v 'metrics\|counter\|gauge\|histogram'
# Expected: empty output
```

---

## Related

- [[Logging Observability Plan]] — master plan and design decisions
- [[01-span-instrumentation]] — Phase 1 (span context for the new warn lines)
- [[Logging Observability Data Flow]] — shows where these logs appear in the flow
- [[04-production-structured-logging]] — Phase 4 (JSON output of these warn lines)
