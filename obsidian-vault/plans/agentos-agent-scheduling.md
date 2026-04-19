---
title: Agent Scheduling Tools
tags:
  - kernel
  - tools
  - agents
  - v3
date: 2026-04-19
status: complete
effort: 1d
priority: high
---

# Agent Scheduling Tools

> Give agents the ability to schedule one-shot tasks at a relative delay ("in 2 minutes") or absolute datetime ("at 3pm"), with kernel-side firing and user notification.

---

## Problem

- `set-timer` tool exists but the backend is broken — `schedule_manager` is missing `create_timer`, `list_timers`, `cancel_timer_by_name`, and `TimerAction` type is undefined.
- No absolute-datetime scheduling ("at 3pm", "next Tuesday") — only relative `delay_secs`.
- Timer firing (notification delivery + task launch) is not wired into `agentd_loop`.
- No `SetTimer` or `ScheduleOnce` kernel actions exist in `kernel_action.rs`.

## Decision

Fix the broken timer backend and add `schedule-once` for absolute datetime scheduling.

| Tool | Input | Persistence | Surviving restart? |
|------|-------|-------------|-------------------|
| `set-timer` (fixed) | `delay_secs` | in-memory | No |
| `schedule-once` (new) | `fire_at` (ISO8601) or `delay_secs` | in-memory | No (same as timers) |

Note: Both are in-memory. Restart survival requires SQLite persistence (future work). For now, agents should re-register timers on startup if needed.

## Implementation

### Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/schedule.rs` | Add `TimerAction`, `TimerEntry`, `OnceJob`, `OnceJobState` |
| `crates/agentos-kernel/src/schedule_manager.rs` | Add timer + once-job fields + 8 new methods |
| `crates/agentos-kernel/src/run_loop.rs` | Extend `agentd_loop` to fire due timers + once jobs |
| `crates/agentos-kernel/src/kernel_action.rs` | Add `SetTimer` + `ScheduleOnce` variants, wire through |
| `crates/agentos-kernel/src/commands/schedule.rs` | Add `cmd_create_schedule_once` |
| `crates/agentos-tools/src/schedule_once.rs` | New `schedule-once` tool |
| `crates/agentos-tools/src/lib.rs` | `pub mod schedule_once` |
| `crates/agentos-tools/src/runner.rs` | Register `ScheduleOnceTool` |
| `crates/agentos-tools/src/factory.rs` | Add to `KERNEL_CONTEXT_TOOL_NAMES` |
| `tools/core/schedule-once.toml` | New manifest |

## Related
[[Issues and Fixes]]
