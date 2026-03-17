---
title: Graceful Shutdown and Audit Trail
tags:
  - kernel
  - reliability
  - plan
  - v3
date: 2026-03-17
status: planned
effort: 1d
priority: critical
---

# Phase 03 -- Graceful Shutdown and Audit Trail

> Ensure every kernel exit path writes a `KernelShutdown` audit entry, including supervisor loop exits and signal-based shutdowns.

---

## Why This Phase

Audit log analysis shows 5 `KernelStarted` events but 0 `KernelStopped` events in a 50-minute window. The only code path that emits `KernelShutdown` is the explicit `KernelCommand::Shutdown` handler in `run_loop.rs:1151`. When the supervisor exits due to restart budget exhaustion, signal handling, or an "all tasks exited" condition, no shutdown audit entry is written. This makes crash forensics impossible -- you cannot distinguish a clean stop from a crash.

The fix adds `KernelShutdown` audit entries to all exit paths in the supervisor `run()` method.

## Sub-tasks

| # | Task | Files | Detail Doc |
|---|------|-------|------------|
| 05 | Graceful shutdown audit trail | `run_loop.rs`, `kernel.rs` | [[24-05-Graceful Shutdown Audit Trail]] |

## Test Plan

- Confirm the `KernelShutdown` audit event type already exists in `agentos-audit/src/log.rs` (it does: line 56)
- Unit test: after `cancellation_token.cancel()`, the supervisor loop writes a `KernelShutdown` entry before returning
- Test: `Kernel::shutdown()` emits an audit entry before cancelling the token

## Verification

```bash
cargo test -p agentos-kernel -- run_loop
cargo clippy -p agentos-kernel -- -D warnings
```
