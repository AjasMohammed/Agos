---
title: Agent Async Background Tasks
tags:
  - kernel
  - tools
  - agents
  - v3
date: 2026-04-19
status: in-progress
effort: 0.5d
priority: high
---

# Agent Async Background Tasks

> Enable agents to spawn fire-and-forget background tasks, receive automatic completion notifications, and check task status — without blocking the spawning agent.

---

## Problem

Agents currently have two modes for delegating work:
- `task-delegate` — creates a child task and adds a scheduler dependency; the parent waits.
- `agent-call` — synchronous RPC; blocks until target completes.

Neither allows a pattern like: *"spawn this task, continue doing other work, get notified when it finishes."* This is critical for long-running orchestrators that need to fan out work without stalling.

## Options Considered

| Option | Tradeoff |
|--------|----------|
| Extend `task-delegate` with `detach: bool` | Reuses infrastructure but changes semantics of an existing tool (risky) |
| New `task_spawn_async` tool + `SpawnAsync` kernel action | Clean, additive, no existing behavior changes |
| Background pool (BackgroundTask) | Already exists for CLI-initiated jobs; adds complexity for agent-to-agent spawning |

## Decision

New `task-spawn-async` tool emitting a `SpawnAsync` kernel action. The kernel creates a child `AgentTask` with:
- `parent_task_id = Some(spawner_task.id)` — reuses existing `inject_sub_agent_result` for notification
- `spawner_agent_id = Some(spawner_task.agent_id)` — future ownership-based status queries
- **No dependency added** — parent task is never blocked

Existing mechanisms handle notification and status:
- **Notification**: `task_completion.rs` already calls `inject_sub_agent_result` for any task with `parent_task_id` set.
- **Status**: existing `poll-agent` tool works for checking child state within same task lifetime.

## Consequences

- Agents can fan out parallel work and respond to completions as they arrive.
- No changes to `task_completion.rs` or `NotificationRouter` needed.
- Spawned tasks show up in `agentos task list` and `poll-agent` results normally.
- Cross-task polling (agent task B finishes, agent checks from task C) deferred — `spawner_agent_id` field reserved for that use.

## Implementation

### Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/task.rs` | Add `spawner_agent_id: Option<AgentID>` to `AgentTask` |
| `crates/agentos-tools/src/task_spawn_async.rs` | New tool |
| `crates/agentos-tools/src/lib.rs` | `pub mod task_spawn_async` |
| `crates/agentos-tools/src/runner.rs` | Register `TaskSpawnAsyncTool` |
| `crates/agentos-tools/src/factory.rs` | Add `"task-spawn-async"` to `KERNEL_CONTEXT_TOOL_NAMES` |
| `crates/agentos-kernel/src/kernel_action.rs` | `SpawnAsync` variant + handler |
| `crates/agentos-kernel/src/commands/task.rs` | `handle_spawn_async()` |
| `tools/core/task-spawn-async.toml` | Tool manifest |

## Related
[[Issues and Fixes]]
