---
title: Subsystem Restart Hardening
tags:
  - kernel
  - reliability
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 1d
priority: high
---

# Phase 06 -- Subsystem Restart Hardening

> Replace the flat restart budget with exponential backoff, add a circuit breaker per subsystem, and improve panic diagnostics.

---

## Why This Phase

The current supervisor in `run_loop.rs` uses a flat restart budget: 5 restarts within 60 seconds, then full shutdown. This has two problems:

1. **No delay between restarts.** A subsystem that fails due to a transient condition (e.g., temporary disk pressure) is restarted immediately, exhausting the budget in seconds. With exponential backoff, the subsystem would survive the transient.

2. **All-or-nothing shutdown.** When one subsystem exceeds its budget, the entire kernel shuts down. A circuit breaker pattern would mark that specific subsystem as degraded (emitting `KernelSubsystemError`) while keeping the rest of the kernel running.

## Sub-tasks

| # | Task | Files | Detail Doc |
|---|------|-------|------------|
| 08 | Exponential restart backoff | `run_loop.rs` | [[24-08-Exponential Restart Backoff]] |

## Test Plan

- Unit test: `calculate_restart_delay()` returns exponentially increasing delays with jitter
- Unit test: after `MAX_RESTARTS`, the circuit breaker trips and returns `CircuitOpen`
- Unit test: a successful run resets the circuit breaker state
- Test: the supervisor log includes the backoff delay for each restart

## Verification

```bash
cargo test -p agentos-kernel -- restart
cargo clippy -p agentos-kernel -- -D warnings
```
