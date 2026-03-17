---
title: Production Stability Fixes
tags:
  - kernel
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 8d
priority: critical
---

# Production Stability Fixes

> Fix three critical production issues from audit log analysis: memory retrieval failures, health monitor spam, and kernel restart instability.

---

## Current State

Audit log analysis (96 entries, 50 minutes) shows: 100% event-triggered task failure due to empty memory retrieval treated as error; 50+ DiskSpaceLow noise events per hour; 4 unclean restarts with no KernelStopped trail and agent identity lost each time.

## Goal / Target State

Kernel runs for weeks unattended: new agents bootstrap without false failures, health events are aggregated and debounced across restarts, shutdown always leaves an audit trail, agent identity survives restarts, and boot validates system health before starting subsystems.

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[24-01-Retrieval Result Typing]] | `retrieval_gate.rs` | planned |
| 02 | [[24-02-Skip Retrieval for Bootstrap Tasks]] | `task_executor.rs` | planned |
| 03 | [[24-03-Aggregate Disk Health Events]] | `health_monitor.rs` | planned |
| 04 | [[24-04-Persist Health Debounce State]] | `health_monitor.rs`, `config.rs` | planned |
| 05 | [[24-05-Graceful Shutdown Audit Trail]] | `run_loop.rs`, `kernel.rs` | planned |
| 06 | [[24-06-Agent Identity Reuse on Reconnect]] | `commands/agent.rs`, `agent_registry.rs` | planned |
| 07 | [[24-07-Boot Pre-flight Checks]] | `kernel.rs`, `config.rs` | planned |
| 08 | [[24-08-Exponential Restart Backoff]] | `run_loop.rs` | planned |
| 09 | [[24-09-Systemd Unit and Watchdog]] | `deploy/agentos.service` (new), `health.rs` | planned |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

## Related

[[Production Stability Fixes Plan]], [[Issues and Fixes]]
