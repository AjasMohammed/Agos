---
title: Persist Health Debounce State
tags:
  - kernel
  - health
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 4h
priority: high
---

# Persist Health Debounce State

> Persist health monitor debounce timestamps to a file so the 10-minute debounce window survives kernel restarts, preventing a burst of events on every boot.

---

## Why This Subtask

The debounce state in `health_monitor.rs` is stored in `HashMap<String, Instant>` (line 36). `Instant` is a monotonic clock that cannot be serialized or compared across process lifetimes. Every kernel restart resets the debounce map, causing an immediate burst of all threshold-exceeding events.

The fix replaces the in-memory `Instant`-based debounce with a `DateTime<Utc>`-based approach, persisting the last-emitted timestamps to a JSON file in the data directory. On startup, the health monitor loads this file and skips any events whose debounce window has not expired.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Debounce storage | `HashMap<String, Instant>` (in-memory) | `HashMap<String, DateTime<Utc>>` persisted to `{data_dir}/health_debounce.json` |
| Cross-restart behavior | Full event burst on every boot | Events suppressed if debounce window is still active from previous run |
| Clock type | `std::time::Instant` (monotonic, non-serializable) | `chrono::DateTime<Utc>` (wall clock, serializable) |
| Persistence trigger | Never | After each event emission (async, non-blocking) |

## What to Do

1. Open `crates/agentos-kernel/src/health_monitor.rs`

2. Change the debounce map type from `HashMap<String, Instant>` to `HashMap<String, chrono::DateTime<chrono::Utc>>`. Add `use chrono::Utc;` at the top.

3. Add a `debounce_file_path` parameter to `run_health_monitor` and `check_system_health`. The path is `{data_dir}/health_debounce.json`. Pass it from `Kernel` (which knows `data_dir`).

4. Update `should_emit()` to use `DateTime<Utc>`:

```rust
fn should_emit(
    last_emitted: &mut HashMap<String, chrono::DateTime<chrono::Utc>>,
    key: &str,
    debounce_secs: u64,
) -> bool {
    let now = chrono::Utc::now();
    let debounce = chrono::Duration::seconds(debounce_secs as i64);
    match last_emitted.get(key) {
        Some(last) if now.signed_duration_since(*last) < debounce => false,
        _ => {
            last_emitted.insert(key.to_string(), now);
            true
        }
    }
}
```

5. Add load/save helper functions:

```rust
fn load_debounce_state(path: &std::path::Path) -> HashMap<String, chrono::DateTime<chrono::Utc>> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_debounce_state(
    path: &std::path::Path,
    state: &HashMap<String, chrono::DateTime<chrono::Utc>>,
) {
    if let Ok(json) = serde_json::to_string_pretty(state) {
        if let Err(e) = std::fs::write(path, json) {
            tracing::warn!(error = %e, "Failed to persist health debounce state");
        }
    }
}
```

6. In `run_health_monitor()`, load the debounce state at startup and save it after each `check_system_health()` call:

```rust
let debounce_path = kernel.data_dir.join("health_debounce.json");
let mut last_emitted = load_debounce_state(&debounce_path);

loop {
    tokio::select! {
        _ = cancellation.cancelled() => break,
        _ = tokio::time::sleep(interval) => {
            check_system_health(&kernel, &thresholds, &permissions, &mut last_emitted).await;
            // Persist asynchronously (fire-and-forget on blocking thread)
            let state = last_emitted.clone();
            let path = debounce_path.clone();
            tokio::task::spawn_blocking(move || save_debounce_state(&path, &state));
        }
    }
}
```

7. Update `run_health_monitor` signature to accept the kernel's `data_dir` path. In `run_loop.rs`, the health monitor task spawner already has `kernel: Arc<Kernel>`, and `kernel.data_dir` is a `PathBuf` -- so no additional plumbing is needed; the health monitor function can access it directly via `kernel.data_dir`.

8. Update all `should_emit()` call sites to pass `DEBOUNCE_INTERVAL_SECS`:
```rust
// Before:
should_emit(last_emitted, "CPUSpikeDetected")
// After:
should_emit(last_emitted, "CPUSpikeDetected", DEBOUNCE_INTERVAL_SECS)
```

9. Update the existing test `hal_read_permissions_grants_hardware_system_read` if needed (it does not use debounce, so no change required).

10. Add new tests:

```rust
#[test]
fn should_emit_respects_debounce_window() {
    let mut state: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    // First call should emit
    assert!(should_emit(&mut state, "TestEvent", 600));
    // Second call within window should not emit
    assert!(!should_emit(&mut state, "TestEvent", 600));
}

#[test]
fn load_save_debounce_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("debounce.json");

    let mut state = HashMap::new();
    state.insert("CPUSpikeDetected".to_string(), chrono::Utc::now());
    save_debounce_state(&path, &state);

    let loaded = load_debounce_state(&path);
    assert!(loaded.contains_key("CPUSpikeDetected"));
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/health_monitor.rs` | Change debounce map to `DateTime<Utc>`, add load/save persistence, update `should_emit()` signature |

## Prerequisites

[[24-03-Aggregate Disk Health Events]] should be complete first (it changes the same file region). However, they can be done in either order if you merge carefully.

## Test Plan

- `cargo test -p agentos-kernel -- health` passes
- New tests: `should_emit_respects_debounce_window`, `load_save_debounce_roundtrip`
- Manual: restart kernel twice, check audit log for absence of event burst on second start

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- health --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
