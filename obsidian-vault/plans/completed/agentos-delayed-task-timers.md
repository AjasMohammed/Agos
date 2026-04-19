---
title: Delayed Task Timers (One-Shot Scheduling)
tags:
  - kernel
  - tools
  - scheduling
  - v3
  - plan
date: 2026-04-14
status: complete
effort: 4h
priority: high
---

# Delayed Task Timers (One-Shot Scheduling)

> Allow agents to schedule one-shot tasks that fire after a delay — enabling reminders, deferred notifications, and timed follow-ups without cron expressions.

---

## Problem

AgentOS has a full cron scheduling system (`ScheduleManager`) for recurring jobs, but agents cannot schedule a simple one-shot delayed task like "send the user a reminder in 2 minutes." The `notify-user` tool fires immediately, and the only workaround is to spawn a background agent that sleeps — resource-intensive and unreliable.

## Current State

| Component | Exists | Gap |
|-----------|--------|-----|
| `ScheduleManager` (cron jobs) | Yes | No one-shot timer support |
| `agentd_loop` (1s poll) | Yes | Only checks cron `check_due_jobs()` |
| `notify-user` tool | Yes | Immediate only, no delay parameter |
| `KernelAction::NotifyUser` | Yes | No deferred variant |
| `BackgroundPool` | Yes | Could work but wastes an LLM inference cycle |

## Decision

Extend `ScheduleManager` with a lightweight `DelayedTask` registry. Add a `SetTimer` kernel action and a `set-timer` tool. When a timer fires, the kernel creates a background task (which can notify the user, run a prompt, or both). This reuses the existing 1-second `agentd_loop` poll — no new async loops needed.

### Why not a new TimerManager?

The `ScheduleManager` already owns the `agentd_loop` tick. Adding a parallel `HashMap<TimerID, DelayedTask>` with `check_due_timers()` keeps the architecture simple — one component, one loop, both cron and one-shot.

## Design

### `DelayedTask` struct (in `agentos-types/src/schedule.rs`)

```rust
pub struct DelayedTask {
    pub id: TimerID,            // new ID newtype
    pub name: String,
    pub fire_at: DateTime<Utc>,
    pub agent_name: String,
    pub action: TimerAction,
    pub created_at: DateTime<Utc>,
    pub created_by_task: Option<TaskID>,
    pub fired: bool,
}

pub enum TimerAction {
    /// Run a task prompt on the target agent
    RunTask { prompt: String },
    /// Send a notification to the user
    NotifyUser { subject: String, body: String, priority: String },
    /// Both: run the task AND notify the user
    RunTaskAndNotify { prompt: String, subject: String, body: String },
}
```

### `SetTimer` kernel action (tool → kernel)

```rust
KernelAction::SetTimer {
    name: String,
    delay_secs: u64,
    agent_name: String,
    action: TimerAction,
}
```

### `set-timer` tool

Input: `{ "name": "reminder", "delay_secs": 120, "action": "notify", "subject": "Reminder", "body": "2 minutes have passed" }`
Returns: `{ "_kernel_action": "set_timer", ... }`

### Fire logic (in `agentd_loop`)

After `check_due_jobs()`, call `check_due_timers()`. For each due timer:
- `NotifyUser` → push to `NotificationRouter`
- `RunTask` → `create_background_task()`
- `RunTaskAndNotify` → both

Emit `TimerFired` audit event. Remove timer after firing.

## Files Changed

| File | Change |
|------|--------|
| `agentos-types/src/schedule.rs` | Add `DelayedTask`, `TimerAction`, `TimerID` |
| `agentos-types/src/ids.rs` | Add `TimerID` via `define_id!()` |
| `agentos-types/src/lib.rs` | Re-export `TimerID` |
| `agentos-kernel/src/schedule_manager.rs` | Add timer HashMap, `create_timer()`, `cancel_timer()`, `list_timers()`, `check_due_timers()` |
| `agentos-kernel/src/kernel_action.rs` | Add `SetTimer` variant + dispatch |
| `agentos-kernel/src/run_loop.rs` | Call `check_due_timers()` in `agentd_loop` |
| `agentos-tools/src/set_timer.rs` | New `SetTimerTool` |
| `agentos-tools/src/factory.rs` | Register in `KERNEL_CONTEXT_TOOL_NAMES` |
| `agentos-tools/src/runner.rs` | Instantiate `SetTimerTool` |
| `agentos-tools/src/lib.rs` | `pub mod set_timer` |
| `tools/core/set-timer.toml` | Tool manifest |
| `agentos-bus/src/message.rs` | Add `ListTimers`, `CancelTimer` commands |
| `agentos-kernel/src/commands/` | Timer command handlers |
| `agentos-cli/src/commands/` | `agentos timer list/cancel` CLI |
| `agentos-audit/src/lib.rs` | Add `TimerCreated`, `TimerFired`, `TimerCancelled` event types |

## Consequences

- Agents can natively handle "remind me in X minutes" requests
- No LLM inference wasted on sleeping background agents
- Timer resolution is 1 second (matches `agentd_loop` tick)
- Timers are in-memory only (lost on kernel restart — acceptable for short delays; future: persist to SQLite)

## Related

- [[Schedule Manager]] (cron jobs)
- [[Background Tasks]] (background pool)
- [[Notification Router]] (user notifications)
