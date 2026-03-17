---
title: Boot Pre-flight Checks
tags:
  - kernel
  - reliability
  - plan
  - v3
date: 2026-03-17
status: planned
effort: 1d
priority: high
---

# Phase 05 -- Boot Pre-flight Checks

> Add system health validation at the start of `Kernel::boot()` to prevent crash loops when the system is degraded (low disk, corrupt DB, inaccessible vault).

---

## Why This Phase

The kernel's 4 unclean restarts in 50 minutes were likely caused by a cascading failure: disk pressure causes SQLite WAL checkpoint failures, which crash a subsystem, which triggers a restart, which encounters the same disk pressure. The kernel currently has no pre-flight validation -- it initializes subsystems sequentially and only discovers problems when they fail.

Adding pre-flight checks at the top of `Kernel::boot()` will:
1. Verify minimum free disk space on the data directory partition
2. Test that the audit DB and vault DB paths are writable
3. Log clear diagnostic messages to stderr (before the audit log is open)
4. Return a descriptive error instead of crashing deep in subsystem init

## Sub-tasks

| # | Task | Files | Detail Doc |
|---|------|-------|------------|
| 07 | Boot pre-flight checks | `kernel.rs`, `config.rs` | [[24-07-Boot Pre-flight Checks]] |

## Test Plan

- Unit test: `preflight_check_disk_space()` returns an error when free space is below threshold
- Unit test: `preflight_check_db_writable()` returns an error for a non-existent directory
- Test: `Kernel::boot()` fails with a clear error message when pre-flight checks fail

## Verification

```bash
cargo test -p agentos-kernel -- preflight
cargo clippy -p agentos-kernel -- -D warnings
```
