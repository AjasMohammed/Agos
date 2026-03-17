---
title: Exponential Restart Backoff
tags:
  - kernel
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: complete
effort: 4h
priority: high
---

# Exponential Restart Backoff

> Replace the flat restart budget in the supervisor with exponential backoff delays and a circuit breaker that degrades individual subsystems instead of shutting down the entire kernel.

---

## Why This Subtask

The supervisor in `crates/agentos-kernel/src/run_loop.rs` has a flat restart policy: 5 restarts within 60 seconds, then the entire kernel shuts down (lines 42-44, 734-755). This means:

1. A subsystem that fails due to a 5-second transient (e.g., temporary disk pressure) burns through all 5 restarts in ~5 seconds and kills the kernel.
2. A non-critical subsystem (e.g., `Consolidation`) exceeding its budget kills critical subsystems (e.g., `Acceptor`, `Executor`).

The fix adds:
- Exponential backoff delay between restarts: `min(base_ms * 2^attempt, max_delay_ms)` with jitter
- Per-subsystem circuit breaker: when a subsystem exceeds its budget, it is marked as degraded but the kernel keeps running with remaining subsystems
- Critical vs non-critical subsystem distinction: only critical subsystem budget exhaustion shuts down the kernel

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Restart delay | 0 (immediate) | `min(500ms * 2^attempt, 30s) + random_jitter(0-500ms)` |
| Restart budget | 5 restarts / 60s (global per task name) | 5 restarts / 60s (per task, unchanged) but with backoff delay |
| Budget exhaustion | Entire kernel shuts down | Critical: kernel shuts down. Non-critical: subsystem marked degraded, rest continue |
| Critical subsystems | Not classified | `Acceptor`, `Executor`, `TimeoutChecker`, `EventDispatcher` |
| Non-critical subsystems | Not classified | `Consolidation`, `HealthMonitor`, `CommNotificationListener`, `ScheduleNotificationListener`, `ArbiterNotificationListener`, `ToolLifecycleListener`, `Scheduler` |
| Restart state | `(u32, Instant)` count + window start | `SubsystemState { attempt: u32, window_start: Instant, circuit_open: bool }` |

## What to Do

1. Open `crates/agentos-kernel/src/run_loop.rs`

2. Add a `SubsystemState` struct and classification function below the existing constants (around line 44):

```rust
/// Per-subsystem restart tracking with exponential backoff.
struct SubsystemState {
    attempt: u32,
    window_start: std::time::Instant,
    circuit_open: bool,
}

impl SubsystemState {
    fn new() -> Self {
        Self {
            attempt: 0,
            window_start: std::time::Instant::now(),
            circuit_open: false,
        }
    }
}

/// Base delay for exponential backoff (milliseconds).
const BACKOFF_BASE_MS: u64 = 500;
/// Maximum delay between restarts (milliseconds).
const BACKOFF_MAX_MS: u64 = 30_000;

impl TaskKind {
    /// Returns true for subsystems whose failure should shut down the kernel.
    fn is_critical(&self) -> bool {
        matches!(
            self,
            TaskKind::Acceptor
                | TaskKind::Executor
                | TaskKind::TimeoutChecker
                | TaskKind::EventDispatcher
        )
    }
}

/// Calculate the backoff delay for a given attempt number.
/// Uses exponential backoff with jitter: min(base * 2^attempt, max) + random(0..500ms)
fn calculate_restart_delay(attempt: u32) -> Duration {
    let base = BACKOFF_BASE_MS.saturating_mul(1u64.saturating_shl(attempt));
    let clamped = base.min(BACKOFF_MAX_MS);
    // Simple jitter: use the attempt number to vary the delay slightly
    // (avoids pulling in a full RNG crate for this one use)
    let jitter_ms = (attempt as u64 * 137) % 500;
    Duration::from_millis(clamped + jitter_ms)
}
```

3. Replace the `restart_counts` HashMap (line 544) with:

```rust
let mut subsystem_states: std::collections::HashMap<String, SubsystemState> =
    std::collections::HashMap::new();
```

4. Replace `check_restart_budget()` (lines 734-755) with a new method that includes backoff:

```rust
/// Check if a task is within its restart budget and calculate backoff delay.
/// Returns Some(delay) if restart is allowed, None if the circuit should open.
fn check_restart_with_backoff(
    &self,
    states: &mut std::collections::HashMap<String, SubsystemState>,
    task_name: &str,
) -> Option<Duration> {
    let now = std::time::Instant::now();
    let state = states
        .entry(task_name.to_string())
        .or_insert_with(SubsystemState::new);

    // If circuit is already open, reject immediately
    if state.circuit_open {
        return None;
    }

    // Reset counter if outside the window
    if now.duration_since(state.window_start) > Duration::from_secs(RESTART_WINDOW_SECS) {
        state.attempt = 0;
        state.window_start = now;
    }

    state.attempt += 1;
    if state.attempt > MAX_RESTARTS {
        state.circuit_open = true;
        return None;
    }

    Some(calculate_restart_delay(state.attempt - 1))
}
```

5. Update the supervisor loop to use backoff delays. In the `Some(Ok(kind))` branch (around line 585), replace the `check_restart_budget` call:

```rust
Some(Ok(kind)) => {
    tracing::warn!(task = %kind, "Kernel task exited unexpectedly");
    // ... existing audit entry ...

    match self.check_restart_with_backoff(&mut subsystem_states, &kind.to_string()) {
        Some(delay) => {
            tracing::info!(
                task = %kind,
                delay_ms = delay.as_millis() as u64,
                attempt = subsystem_states.get(&kind.to_string()).map(|s| s.attempt).unwrap_or(0),
                "Restarting task with backoff"
            );
            tokio::time::sleep(delay).await;
            Self::spawn_tracked_task(
                &mut join_set,
                &mut task_id_map,
                kind,
                self.clone(),
            );
        }
        None => {
            // Circuit open for this subsystem
            if kind.is_critical() {
                self.emit_event(/* KernelSubsystemError ... */).await;
                tracing::error!(task = %kind, "Critical task exceeded restart budget, kernel shutting down");
                self.audit_shutdown(&format!("critical_restart_budget_exceeded:{}", kind));
                break;
            } else {
                // Non-critical: mark degraded but keep running
                tracing::error!(
                    task = %kind,
                    "Non-critical task exceeded restart budget, marking subsystem degraded"
                );
                self.emit_event(
                    agentos_types::EventType::KernelSubsystemError,
                    agentos_types::EventSource::InferenceKernel,
                    agentos_types::EventSeverity::Warning,
                    serde_json::json!({
                        "task_kind": kind.to_string(),
                        "reason": "restart_budget_exceeded_degraded",
                        "max_restarts": MAX_RESTARTS,
                    }),
                    0,
                ).await;
                // Do NOT break -- continue running other subsystems
            }
        }
    }
}
```

6. Apply the same pattern to the `Some(Err(join_error))` branch (around line 630).

7. Add unit tests:

```rust
#[test]
fn calculate_restart_delay_is_exponential() {
    let d0 = calculate_restart_delay(0);
    let d1 = calculate_restart_delay(1);
    let d2 = calculate_restart_delay(2);
    // Each delay should roughly double (plus jitter)
    assert!(d1.as_millis() > d0.as_millis());
    assert!(d2.as_millis() > d1.as_millis());
}

#[test]
fn calculate_restart_delay_is_capped() {
    let d_max = calculate_restart_delay(100);
    // Should be capped at BACKOFF_MAX_MS + jitter
    assert!(d_max.as_millis() <= (BACKOFF_MAX_MS + 500) as u128);
}

#[test]
fn task_kind_critical_classification() {
    assert!(TaskKind::Acceptor.is_critical());
    assert!(TaskKind::Executor.is_critical());
    assert!(!TaskKind::Consolidation.is_critical());
    assert!(!TaskKind::HealthMonitor.is_critical());
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/run_loop.rs` | Add `SubsystemState`, `calculate_restart_delay()`, `is_critical()` classification; replace flat budget with exponential backoff + circuit breaker |

## Prerequisites

None -- this subtask is independent. However, it pairs well with [[24-05-Graceful Shutdown Audit Trail]] since the shutdown audit helper `audit_shutdown()` is used in the critical-subsystem exit path.

## Test Plan

- `cargo test -p agentos-kernel -- restart` passes
- New tests: `calculate_restart_delay_is_exponential`, `calculate_restart_delay_is_capped`, `task_kind_critical_classification`
- Verify by code review: non-critical subsystem budget exhaustion does not `break` from the supervisor loop

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- restart --nocapture
cargo test -p agentos-kernel -- task_kind --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
