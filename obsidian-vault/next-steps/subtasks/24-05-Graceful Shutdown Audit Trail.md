---
title: Graceful Shutdown Audit Trail
tags:
  - kernel
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 3h
priority: critical
---

# Graceful Shutdown Audit Trail

> Ensure every kernel exit path writes a `KernelShutdown` audit entry, including supervisor loop termination, restart budget exhaustion, and signal-based shutdown.

---

## Why This Subtask

Audit log analysis shows 5 `KernelStarted` events but 0 corresponding `KernelShutdown` events. The only code that emits `KernelShutdown` is the explicit `KernelCommand::Shutdown` handler in `run_loop.rs` (line 1151-1169). When the kernel exits via the supervisor loop (restart budget exceeded, all tasks exited, or `cancellation_token.cancelled()`), no shutdown audit entry is written.

Without `KernelShutdown` entries, it is impossible to distinguish a clean stop from a crash in the audit trail.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `KernelCommand::Shutdown` handler | Emits `KernelShutdown`, cancels token | No change (already correct) |
| Supervisor `cancellation_token.cancelled()` | Logs to tracing, aborts tasks | Also writes `KernelShutdown` audit entry with reason "signal_shutdown" |
| Supervisor restart budget exceeded | Emits `KernelSubsystemError` event, breaks | Also writes `KernelShutdown` audit entry with reason "restart_budget_exceeded" |
| Supervisor "all tasks exited" | Logs error, breaks | Also writes `KernelShutdown` audit entry with reason "all_tasks_exited" |
| `Kernel::shutdown()` method | Cancels token only | Also writes `KernelShutdown` audit entry before cancelling token |

## What to Do

1. Open `crates/agentos-kernel/src/kernel.rs`

2. Update `Kernel::shutdown()` (line 498-500) to emit an audit entry before cancelling:

```rust
pub fn shutdown(&self) {
    self.audit_log(agentos_audit::AuditEntry {
        timestamp: chrono::Utc::now(),
        trace_id: agentos_types::TraceID::new(),
        event_type: agentos_audit::AuditEventType::KernelShutdown,
        agent_id: None,
        task_id: None,
        tool_id: None,
        details: serde_json::json!({
            "reason": "api_shutdown",
            "uptime_secs": chrono::Utc::now()
                .signed_duration_since(self.started_at)
                .num_seconds(),
        }),
        severity: agentos_audit::AuditSeverity::Info,
        reversible: false,
        rollback_ref: None,
    });
    self.cancellation_token.cancel();
}
```

3. Open `crates/agentos-kernel/src/run_loop.rs`

4. Add a helper method for the shutdown audit entry to avoid duplication:

```rust
/// Write a KernelShutdown audit entry with the given reason.
fn audit_shutdown(&self, reason: &str) {
    self.audit_log(agentos_audit::AuditEntry {
        timestamp: chrono::Utc::now(),
        trace_id: agentos_types::TraceID::new(),
        event_type: agentos_audit::AuditEventType::KernelShutdown,
        agent_id: None,
        task_id: None,
        tool_id: None,
        details: serde_json::json!({
            "reason": reason,
            "uptime_secs": chrono::Utc::now()
                .signed_duration_since(self.started_at)
                .num_seconds(),
        }),
        severity: agentos_audit::AuditSeverity::Info,
        reversible: false,
        rollback_ref: None,
    });
}
```

5. In the `run()` method's supervisor loop, add `audit_shutdown()` calls at each exit point:

   a. **Cancellation (line 577-581):**
   ```rust
   _ = self.cancellation_token.cancelled() => {
       tracing::info!("Kernel shutdown requested, stopping supervisor");
       self.audit_shutdown("signal_shutdown");
       join_set.abort_all();
       break;
   }
   ```

   b. **Restart budget exceeded for normal exit (around line 626-628):**
   ```rust
   tracing::error!(task = %kind, "Task exceeded restart budget, kernel degraded");
   self.audit_shutdown(&format!("restart_budget_exceeded:{}", kind));
   break;
   ```

   c. **Restart budget exceeded for panic (around line 718-719):**
   ```rust
   tracing::error!("Kernel exceeded restart budget, shutting down");
   self.audit_shutdown(&format!("restart_budget_exceeded:{}", task_name));
   break;
   ```

   d. **All tasks exited (around line 724-726):**
   ```rust
   None => {
       tracing::error!("All kernel tasks exited, shutting down");
       self.audit_shutdown("all_tasks_exited");
       break;
   }
   ```

6. In the existing `KernelCommand::Shutdown` handler (line 1151-1169), keep the existing audit entry but update it to also include `uptime_secs`:
```rust
details: serde_json::json!({
    "reason": "shutdown_command",
    "uptime_secs": chrono::Utc::now()
        .signed_duration_since(self.started_at)
        .num_seconds(),
}),
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/kernel.rs` | Add audit entry to `shutdown()` method |
| `crates/agentos-kernel/src/run_loop.rs` | Add `audit_shutdown()` helper; call it at all 4 supervisor exit points |

## Prerequisites

None -- this subtask is independent.

## Test Plan

- `cargo build -p agentos-kernel` compiles cleanly
- `cargo test -p agentos-kernel` -- all existing tests pass
- Verify by code review: every `break` in the supervisor `run()` loop is preceded by an `audit_shutdown()` call
- Integration: start kernel, call `Kernel::shutdown()`, check audit DB for `KernelShutdown` entry

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
