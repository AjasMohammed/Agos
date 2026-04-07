---
title: "Phase 2: Completion Event Emission"
tags:
  - kernel
  - multi-agent
  - plan
date: 2026-04-03
status: planned
effort: 2d
priority: high
---

# Phase 2: Completion Event Emission

> Emit structured `TaskCompletionEvent` notifications over a Tokio broadcast channel when tasks reach a terminal state, enabling parent tasks to react to child completion without polling.

---

## Why This Phase

Phase 1 makes task execution async, but there is no mechanism for a parent task to be notified when a child completes. Currently, `cmd_await_sub_agents` (line 373 of `sub_agent.rs`) polls `scheduler.get_task()` for each child -- it gets a point-in-time snapshot and returns immediately. If the child is still running, the parent gets `"state=running"` and must call `await_agents` again on its next LLM turn.

This phase adds a broadcast channel that fires when any task reaches a terminal state (`Complete`, `Failed`, `Cancelled`). Phase 3 will use this channel to implement blocking `await_agents` with result injection.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Task completion notification | None; only audit log entry written | `TaskCompletionEvent` emitted to `completion_tx: broadcast::Sender` |
| `Kernel` struct | No completion channel | New `completion_tx` and `_completion_rx` fields |
| Task state transitions | `scheduler.update_state()` / `update_state_if_not_terminal()` | Same, plus `completion_tx.send()` after terminal transitions |
| `TaskCompletionEvent` type | Does not exist | New struct in `agentos-types/src/task.rs` |

## What to Do

### 1. Define `TaskCompletionEvent`

Open `crates/agentos-types/src/task.rs`. Add:

```rust
/// Emitted when a task reaches a terminal state (Complete, Failed, Cancelled).
/// Carried over a Tokio broadcast channel for in-process subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompletionEvent {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    /// The parent task that spawned this task, if any.
    pub parent_task_id: Option<TaskID>,
    pub final_state: TaskState,
    /// The task's final output text (truncated to 8 KiB).
    pub output: String,
    pub completed_at: chrono::DateTime<chrono::Utc>,
}
```

### 2. Re-export from crate root

Open `crates/agentos-types/src/lib.rs`. Add `TaskCompletionEvent` to the `task` re-export line:

```rust
pub use task::{
    AgentBudget, AgentTask, BudgetAction, ComplexityLevel, CostSnapshot, ModelDowngradeTier,
    PreemptionLevel, TaskCompletionEvent, TaskReasoningHints, TaskState, TaskSummary,
};
```

### 3. Add broadcast channel to `Kernel`

Open `crates/agentos-kernel/src/kernel.rs`. Add to the `Kernel` struct:

```rust
/// Broadcast channel for task completion events.
/// Subscribers (e.g., await_agents handler) call `completion_tx.subscribe()`.
pub(crate) completion_tx: tokio::sync::broadcast::Sender<TaskCompletionEvent>,
/// Held to keep the channel alive; never read directly.
_completion_rx: tokio::sync::broadcast::Receiver<TaskCompletionEvent>,
```

In the `Kernel::new()` or `Kernel::boot()` constructor, initialize:

```rust
let (completion_tx, _completion_rx) = tokio::sync::broadcast::channel(256);
```

### 4. Emit events on task completion

Open `crates/agentos-kernel/src/commands/task.rs`. In the async task execution block added in Phase 1, after storing the task result and updating state to `Complete`:

```rust
// Emit completion event for subscribers (e.g., parent tasks waiting via await_agents).
let _ = kernel.completion_tx.send(TaskCompletionEvent {
    task_id: task_clone.id,
    agent_id: task_clone.agent_id,
    parent_task_id: task_clone.parent_task_id,
    final_state: TaskState::Complete,
    output: task_result.answer.chars().take(8192).collect(),
    completed_at: chrono::Utc::now(),
});
```

Similarly, in the error/failure branch:

```rust
let _ = kernel.completion_tx.send(TaskCompletionEvent {
    task_id: task_clone.id,
    agent_id: task_clone.agent_id,
    parent_task_id: task_clone.parent_task_id,
    final_state: TaskState::Failed,
    output: format!("Error: {}", e),
    completed_at: chrono::Utc::now(),
});
```

### 5. Emit on cancellation

Open `crates/agentos-kernel/src/commands/task.rs`. In `cmd_cancel_task`, after the state transition succeeds (line ~253):

```rust
if let Some(ref task) = task_snapshot {
    let _ = self.completion_tx.send(TaskCompletionEvent {
        task_id: task.id,
        agent_id: task.agent_id,
        parent_task_id: task.parent_task_id,
        final_state: TaskState::Cancelled,
        output: "Task cancelled".to_string(),
        completed_at: chrono::Utc::now(),
    });
}
```

### 6. Emit from background task executor

Open `crates/agentos-kernel/src/task_executor.rs`. Search for locations where tasks transition to `Complete` or `Failed` in the background execution path (the `execute_task()` method around line 4257). Add the same `completion_tx.send()` pattern after each terminal state transition.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/task.rs` | Add `TaskCompletionEvent` struct |
| `crates/agentos-types/src/lib.rs` | Re-export `TaskCompletionEvent` |
| `crates/agentos-kernel/src/kernel.rs` | Add `completion_tx` and `_completion_rx` fields; initialize in constructor |
| `crates/agentos-kernel/src/commands/task.rs` | Emit `TaskCompletionEvent` on complete, failed, cancelled transitions |
| `crates/agentos-kernel/src/task_executor.rs` | Emit `TaskCompletionEvent` from background task completion path |

## Prerequisites

[[01-async-task-execution]] must be complete first -- the async execution path is where most completion events originate.

## Test Plan

- **Unit test `test_completion_event_emitted_on_success`:** Subscribe to `completion_tx`, execute a task that completes, assert the event has `final_state == Complete` and `output` matches the task answer.
- **Unit test `test_completion_event_emitted_on_failure`:** Subscribe to `completion_tx`, execute a task that fails (e.g., LLM returns error), assert `final_state == Failed`.
- **Unit test `test_completion_event_emitted_on_cancel`:** Subscribe to `completion_tx`, call `cmd_cancel_task`, assert `final_state == Cancelled`.
- **Unit test `test_completion_event_includes_parent_id`:** Spawn a sub-agent, let it complete, assert the event's `parent_task_id` matches the spawning parent.
- **Unit test `test_broadcast_no_subscribers_does_not_panic`:** Send a completion event with no subscribers; assert no error (the `let _ = send()` pattern absorbs the `RecvError`).

## Verification

```bash
cargo build -p agentos-types -p agentos-kernel
cargo test -p agentos-types -- --nocapture
cargo test -p agentos-kernel -- --nocapture
cargo clippy -p agentos-types -p agentos-kernel -- -D warnings
```
