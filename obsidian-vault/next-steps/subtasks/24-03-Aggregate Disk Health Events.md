---
title: Aggregate Disk Health Events
tags:
  - kernel
  - health
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 3h
priority: high
---

# Aggregate Disk Health Events

> Replace per-mount `DiskSpaceLow` event emission with a single aggregated event containing all affected mount points as an array in the payload.

---

## Why This Subtask

The health monitor in `crates/agentos-kernel/src/health_monitor.rs` iterates over all mounted filesystems (lines 154-210) and emits a separate `DiskSpaceLow` or `DiskSpaceCritical` event for each mount point that exceeds the threshold. With 6 mount points, this produces 6 events per 30-second cycle -- the dominant noise source in the audit log.

The fix collects all affected mounts into vectors and emits one event per severity tier (warning vs critical) per cycle, with the mount details as a JSON array in the payload.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| DiskSpaceLow events per cycle | 1 per affected mount (6 on typical Linux) | 1 total, with `mounts` array in payload |
| DiskSpaceCritical events | 1 per affected mount | 1 total, with `mounts` array in payload |
| Debounce key | `"DiskSpaceLow:{mount}"` (per mount) | `"DiskSpaceLow"` (single key for all mounts) |
| Payload format | `{ disk_percent, mount_point, threshold }` | `{ affected_mount_count, mounts: [{mount, used_percent}], threshold }` |

## What to Do

1. Open `crates/agentos-kernel/src/health_monitor.rs`

2. Replace the disk section (lines 154-210) with an aggregation pattern. Instead of emitting inside the `for disk in disks` loop, collect results first:

```rust
// Disk -- aggregate all affected mounts into single events per severity tier
if let Some(disks) = snapshot.get("disk_usage").and_then(|d| d.as_array()) {
    let mut warning_mounts: Vec<serde_json::Value> = Vec::new();
    let mut critical_mounts: Vec<serde_json::Value> = Vec::new();

    for disk in disks {
        let total = disk
            .get("total_space_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let available = disk
            .get("available_space_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let mount = disk
            .get("mount_point")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        if total == 0 {
            continue;
        }

        let used = total.saturating_sub(available);
        let used_percent = (used as f32 / total as f32) * 100.0;

        if used_percent > thresholds.disk_critical_percent {
            critical_mounts.push(serde_json::json!({
                "mount_point": mount,
                "used_percent": used_percent,
                "total_bytes": total,
                "available_bytes": available,
            }));
        } else if used_percent > thresholds.disk_warning_percent {
            warning_mounts.push(serde_json::json!({
                "mount_point": mount,
                "used_percent": used_percent,
                "total_bytes": total,
                "available_bytes": available,
            }));
        }
    }

    // Emit one DiskSpaceCritical for all critical mounts
    if !critical_mounts.is_empty()
        && should_emit(last_emitted, "DiskSpaceCritical")
    {
        kernel
            .emit_event(
                EventType::DiskSpaceCritical,
                EventSource::HardwareAbstractionLayer,
                EventSeverity::Critical,
                serde_json::json!({
                    "affected_mount_count": critical_mounts.len(),
                    "mounts": critical_mounts,
                    "threshold": thresholds.disk_critical_percent,
                }),
                0,
            )
            .await;
    }

    // Emit one DiskSpaceLow for all warning mounts
    if !warning_mounts.is_empty()
        && should_emit(last_emitted, "DiskSpaceLow")
    {
        kernel
            .emit_event(
                EventType::DiskSpaceLow,
                EventSource::HardwareAbstractionLayer,
                EventSeverity::Warning,
                serde_json::json!({
                    "affected_mount_count": warning_mounts.len(),
                    "mounts": warning_mounts,
                    "threshold": thresholds.disk_warning_percent,
                }),
                0,
            )
            .await;
    }
}
```

3. Note: the debounce key changes from `"DiskSpaceLow:{mount}"` (per-mount) to `"DiskSpaceLow"` (global). This means one debounce timer covers all mounts. If a new mount becomes critical during the debounce window, it will be included in the next cycle's aggregated event after the window expires.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/health_monitor.rs` | Replace per-mount disk event emission with aggregated single-event pattern |

## Prerequisites

None -- this subtask is independent.

## Test Plan

- `cargo test -p agentos-kernel -- health` -- existing `hal_read_permissions_grants_hardware_system_read` test passes
- Add unit test: construct a mock system snapshot JSON with 6 mount points above `disk_warning_percent`, verify that `check_system_health` is called (verify by checking the debounce map has exactly 1 "DiskSpaceLow" key, not 6)
- Verify by code inspection: the `for disk in disks` loop no longer calls `kernel.emit_event()` directly

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- health --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
