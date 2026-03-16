---
title: "Phase 01 — Task Lifecycle Event Emission"
tags:
  - kernel
  - event-system
  - plan
  - v3
date: 2026-03-13
status: in-progress
effort: 3h
priority: high
---
# Phase 01 — Task Lifecycle Event Emission

> Wire TaskStarted, TaskCompleted, TaskFailed, and TaskTimedOut events into the kernel so agents can react to task state changes.

---

## Why This Phase

Task lifecycle is the highest-traffic event category. Every task that runs should emit start/complete/fail events. Without these, supervisor agents and orchestrator agents are blind to task outcomes — they cannot coordinate multi-agent workflows, detect stuck pipelines, or retry failures. This is the single most impactful batch of emission points.

---

## Current State

| What | Status |
|------|--------|
| `EventType::TaskStarted` / `TaskCompleted` / `TaskFailed` / `TaskTimedOut` | Defined in `agentos-types/src/event.rs` |
| `emit_event()` on `Kernel` | Working — used by `AgentAdded`/`AgentRemoved` in `commands/agent.rs` |
| Emission calls in `task_executor.rs` | **None** — no events emitted during task execution |
| Emission calls in `scheduler.rs` for timeouts | **None** |

---

## Target State

- `TaskStarted` emitted at the start of `execute_task()`, after the agent and task are validated
- `TaskCompleted` emitted when the run loop exits successfully (no more tool calls, final answer produced)
- `TaskFailed` emitted on LLM error, budget exceeded, or injection detected — any error path
- `TaskTimedOut` emitted from the `TimeoutChecker` when a task exceeds its timeout

---

## Subtasks

### 1. Add `TaskStarted` emission in `task_executor.rs`

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** Inside `execute_task()`, after the initial validation and before the first inference call. The existing code logs to audit and begins the iteration loop — insert the emit call right before iteration 0.

**Code to add:**

```rust
self.emit_event(
    EventType::TaskStarted,
    EventSource::TaskScheduler,
    EventSeverity::Info,
    serde_json::json!({
        "task_id": task.id.to_string(),
        "agent_id": task.agent_id.to_string(),
        "prompt_preview": task.original_prompt.chars().take(200).collect::<String>(),
    }),
    0,
).await;
```

**Context:** `self` is `Arc<Kernel>`, `task` is `&AgentTask`. Both `task.id` and `task.agent_id` are available.

### 2. Add `TaskCompleted` emission in `task_executor.rs`

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** At the end of `execute_task()`, on the successful exit path — after the iteration loop completes and before the function returns. Look for where `TaskStatus::Completed` is set or where the final audit entry is written.

**Code to add:**

```rust
self.emit_event(
    EventType::TaskCompleted,
    EventSource::TaskScheduler,
    EventSeverity::Info,
    serde_json::json!({
        "task_id": task.id.to_string(),
        "agent_id": task.agent_id.to_string(),
        "iterations": iteration_count,
        "tool_calls": tool_call_count,
    }),
    0,
).await;
```

**Note:** Capture `iteration_count` and `tool_call_count` from the loop variables. If these aren't readily available as named variables, count iterations in the loop.

### 3. Add `TaskFailed` emission in `task_executor.rs`

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** Every error path in `execute_task()`. There are multiple:
- LLM inference error (provider returns error)
- Budget/cost limit exceeded
- Injection detected (high confidence)
- Max iterations exceeded

For each error path, add before the return:

```rust
self.emit_event(
    EventType::TaskFailed,
    EventSource::TaskScheduler,
    EventSeverity::Warning,
    serde_json::json!({
        "task_id": task.id.to_string(),
        "agent_id": task.agent_id.to_string(),
        "reason": "llm_error",  // or "budget_exceeded", "injection_detected", "max_iterations"
        "error": error_message,
    }),
    0,
).await;
```

**Severity:** Use `Warning` for most failures, `Critical` for injection-detected failures.

### 4. Add `TaskTimedOut` emission in `scheduler.rs`

**File:** `crates/agentos-kernel/src/scheduler.rs`

**Where:** In the timeout checking logic. The `TimeoutChecker` or `check_timeouts()` function identifies tasks that have exceeded their timeout. After marking the task as timed out, emit:

```rust
// The scheduler doesn't have direct access to self.emit_event() since it's
// not part of Kernel. Instead, send via the event_sender channel that the
// Kernel passes to the scheduler at construction.
//
// If the scheduler doesn't currently hold an event_sender, add one:
//   event_sender: Option<mpsc::UnboundedSender<EventMessage>>
// and inject it from Kernel::new().

if let Some(ref sender) = self.event_sender {
    let event = EventMessage {
        id: EventID::new(),
        event_type: EventType::TaskTimedOut,
        source: EventSource::TaskScheduler,
        payload: serde_json::json!({
            "task_id": task_id.to_string(),
            "agent_id": agent_id.to_string(),
            "timeout_seconds": timeout_duration.as_secs(),
            "elapsed_seconds": elapsed.as_secs(),
        }),
        severity: EventSeverity::Warning,
        timestamp: Utc::now(),
        signature: vec![],  // Will be signed by emit_event if routed through kernel
        trace_id: uuid::Uuid::new_v4().to_string(),
        chain_depth: 0,
    };
    let _ = sender.send(event);
}
```

**Alternative:** If the timeout check runs inside the kernel context (e.g., in `run_loop.rs` or `health.rs`), use `self.emit_event()` directly. Check which module owns the timeout sweep.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Add 3 `emit_event` calls: TaskStarted, TaskCompleted, TaskFailed |
| `crates/agentos-kernel/src/scheduler.rs` | Add `event_sender` field + `TaskTimedOut` emission in timeout check |
| `crates/agentos-kernel/src/kernel.rs` | Pass `event_sender.clone()` to scheduler during construction (if not already) |

---

## Dependencies

None — this is the first phase. The `emit_event()` infrastructure already works.

---

## Test Plan

1. **Unit test in `task_executor.rs`:** Mock a task execution, verify that `event_sender` receives a `TaskStarted` event with correct `task_id` and `agent_id`.

2. **Unit test for `TaskFailed`:** Trigger an LLM error path, verify `TaskFailed` event is emitted with `reason: "llm_error"`.

3. **Unit test for `TaskTimedOut`:** Set a very short timeout, run a task, verify `TaskTimedOut` event appears.

4. **Integration test:** Boot kernel with a mock agent, submit a task, verify audit log contains `EventEmitted` entries for `TaskStarted` and `TaskCompleted`.

---

## Verification

```bash
# Must compile
cargo build -p agentos-kernel

# All kernel tests pass
cargo test -p agentos-kernel

# Grep to confirm emission points exist
grep -n "TaskStarted" crates/agentos-kernel/src/task_executor.rs
grep -n "TaskCompleted" crates/agentos-kernel/src/task_executor.rs
grep -n "TaskFailed" crates/agentos-kernel/src/task_executor.rs
grep -n "TaskTimedOut" crates/agentos-kernel/src/scheduler.rs
```

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[Event Trigger Completion Data Flow]] — Flow diagram
- [[agentos-event-trigger-system]] — Original spec §3 (TaskLifecycle category)

## Review Fix Pass (2026-03-13)

- Patch paused-task handling so escalation-driven `Waiting` tasks are not overwritten to `Failed`.
- Correct pre-inference budget snapshot label from injection-specific to budget-specific.
- Ensure timeout handling cleans dependency edges and unblocks waiting parents.
- Align timeout failure semantics by emitting `TaskFailed` alongside `TaskTimedOut`.
- Add targeted tests for timeout dependency cleanup and lifecycle failure classification.
- Harden supervisor restart behavior so panicked subsystems restart deterministically.
- Make `EventDispatcher` restart-safe by preserving channel receiver ownership across restarts.
- Align escalation audit metadata (`task_resumed`) with actual approve/deny outcomes.
- Add explicit `AutoAction::Approve` sweep test coverage.
- Normalize escalation audit payload keys across manual resolve and expiry sweep paths.
- Harden expiry sweep fallback: fail task and unblock dependents if auto-approve requeue fails.
- Include stable `agent_id` from escalation records in sweep audit entries.
- Align manual escalation resolve with sweep fallback when approve requeue fails.
