---
title: Health Monitor Consolidation
tags:
  - kernel
  - health
  - reliability
  - plan
  - v3
date: 2026-03-17
status: planned
effort: 1d
priority: high
---

# Phase 02 -- Health Monitor Consolidation

> Aggregate per-mount disk events into a single event per cycle, persist debounce state across restarts, and add escalation tiers for disk pressure.

---

## Why This Phase

The health monitor emits 6 `DiskSpaceLow` events per 30-second check cycle -- one for each mounted filesystem. This is the dominant noise source, consuming over 50% of the audit log. The 10-minute debounce (`DEBOUNCE_INTERVAL_SECS = 600`) is stored in an in-memory `HashMap<String, Instant>` that resets on every kernel restart, so each boot immediately produces a fresh burst of 6 events.

The fix has two parts:
1. Aggregate all affected mounts into a single `DiskSpaceLow` event with an array payload, instead of iterating and emitting per mount.
2. Persist debounce state to the audit SQLite database so the 10-minute window survives restarts.

## Sub-tasks

| # | Task | Files | Detail Doc |
|---|------|-------|------------|
| 03 | Aggregate disk health events | `health_monitor.rs` | [[24-03-Aggregate Disk Health Events]] |
| 04 | Persist health debounce state | `health_monitor.rs`, `config.rs` | [[24-04-Persist Health Debounce State]] |

## Test Plan

- Unit test: given a system snapshot with 6 mount points all above threshold, `check_system_health` emits exactly 1 `DiskSpaceLow` event (not 6)
- Unit test: `should_emit()` returns false when the debounce table has a recent entry
- Unit test: debounce persistence loads correctly on health monitor initialization

## Verification

```bash
cargo test -p agentos-kernel -- health
cargo clippy -p agentos-kernel -- -D warnings
```
