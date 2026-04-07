---
title: "Phase 1: Async Task Execution"
tags:
  - kernel
  - multi-agent
  - plan
date: 2026-04-03
status: planned
effort: 2d
priority: high
---

# Phase 1: Async Task Execution

> Make `cmd_run_task` non-blocking for sub-agent spawns by executing tasks on spawned Tokio tasks, returning `TaskID` immediately while the task runs in the background.

---

## Why This Phase

Currently, `cmd_run_task` in `crates/agentos-kernel/src/commands/task.rs` calls `self.execute_task_sync(&task, &trace_id, &task_span).await` at line 137, which blocks the handler until the task completes. This means:

- The bus connection is occupied for the entire task duration (could be minutes).
- Sub-agent tasks spawned via `cmd_spawn_sub_agent` are enqueued but the parent cannot proceed until its own `cmd_run_task` finishes.
- True multi-agent parallelism is impossible.

The fix is to spawn task execution on a Tokio task and return the `TaskID` immediately. The existing `cmd_run_task` path for interactive CLI usage must still work -- the CLI will poll `task status` until it gets a terminal state.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `cmd_run_task` return | Blocks until task completes; returns full result or error | Returns `TaskID` immediately; task runs on background Tokio task |
| Task execution | `execute_task_sync()` called inline | `execute_task_sync()` called on `tokio::spawn` with `Arc<Kernel>` |
| CLI behavior | Gets result synchronously from bus response | Gets `TaskID` from bus; polls `task status` until terminal |
| `KernelResponse` | `Success { data: { task_id, result } }` or `Error` | New `TaskStarted { task_id }` variant for async path |
| Task result storage | Not stored; returned directly to caller | Stored in `scheduler.task_results: RwLock<HashMap<TaskID, TaskResult>>` |

## What to Do

### 1. Add `TaskStarted` response variant

Open `crates/agentos-bus/src/message.rs`. Add to the `KernelResponse` enum:

```rust
/// A task was started asynchronously. Poll `TaskStatus` for result.
TaskStarted {
    task_id: TaskID,
},
```

### 2. Add task result storage to `TaskScheduler`

Open `crates/agentos-kernel/src/scheduler.rs`. Add a field to `TaskScheduler`:

```rust
pub struct TaskScheduler {
    // ... existing fields ...
    /// Stores final results for completed tasks (answer text, tool call count, iterations).
    /// Populated by the async task execution path; consumed by `cmd_get_task_result`.
    task_results: RwLock<HashMap<TaskID, TaskResultSummary>>,
}
```

Add the summary type and accessor methods:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResultSummary {
    pub answer: String,
    pub tool_call_count: u32,
    pub iterations: u32,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    pub success: bool,
    pub error_message: Option<String>,
}

impl TaskScheduler {
    pub async fn store_task_result(&self, task_id: TaskID, result: TaskResultSummary) {
        self.task_results.write().await.insert(task_id, result);
    }

    pub async fn get_task_result(&self, task_id: &TaskID) -> Option<TaskResultSummary> {
        self.task_results.read().await.get(task_id).cloned()
    }
}
```

### 3. Refactor `cmd_run_task` to spawn async

Open `crates/agentos-kernel/src/commands/task.rs`. Replace the synchronous execution block (lines 128-211) with:

```rust
// Spawn task execution asynchronously.
let kernel = Arc::clone(&self_arc); // requires passing Arc<Kernel> into cmd_run_task
let task_clone = task.clone();
let trace_id_clone = trace_id;
tokio::spawn(async move {
    let task_span = kernel.otel.start_task_span(
        &task_clone.id.to_string(),
        &task_clone.agent_id.to_string(),
        &agent_model,
    );
    kernel.otel.adjust_active_tasks(1);
    let start = std::time::Instant::now();
    
    match kernel.execute_task_sync(&task_clone, &trace_id_clone, &task_span).await {
        Ok(task_result) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            kernel.scheduler
                .update_state_if_not_terminal(&task_clone.id, TaskState::Complete)
                .await.ok();
            kernel.scheduler.store_task_result(task_clone.id, TaskResultSummary {
                answer: task_result.answer,
                tool_call_count: task_result.tool_call_count,
                iterations: task_result.iterations,
                completed_at: chrono::Utc::now(),
                success: true,
                error_message: None,
            }).await;
            kernel.cleanup_task_subscriptions(&task_clone.id).await;
            kernel.trace_collector
                .finish_task(&task_clone.id, "Complete", chrono::Utc::now())
                .await;
            kernel.otel.record_task_metric(
                &task_clone.agent_id.to_string(), "complete", duration_ms
            );
            kernel.otel.adjust_active_tasks(-1);
        }
        Err(e) => {
            // ... mirror existing error handling, storing failure result ...
        }
    }
});

KernelResponse::TaskStarted { task_id: task.id }
```

**Important:** The interactive CLI path needs to remain synchronous. Add a `blocking: bool` field to `KernelCommand::RunTask` (defaulting to `true` for backward compat). When `blocking == true`, keep the existing synchronous path. When `blocking == false` (used by sub-agent spawns and team coordinators), use the async path above.

### 4. Add `KernelCommand::GetTaskResult`

Open `crates/agentos-bus/src/message.rs`. Add:

```rust
GetTaskResult {
    task_id: TaskID,
},
```

### 5. Add result query handler

Open `crates/agentos-kernel/src/commands/task.rs`. Add:

```rust
pub(crate) async fn cmd_get_task_result(&self, task_id: TaskID) -> KernelResponse {
    if let Some(result) = self.scheduler.get_task_result(&task_id).await {
        KernelResponse::Success {
            data: Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "result": result.answer,
                "success": result.success,
                "iterations": result.iterations,
                "tool_calls": result.tool_call_count,
                "error": result.error_message,
            })),
        }
    } else {
        match self.scheduler.get_task(&task_id).await {
            Some(task) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "state": format!("{:?}", task.state),
                })),
            },
            None => KernelResponse::Error {
                message: format!("Task '{}' not found", task_id),
            },
        }
    }
}
```

### 6. Wire dispatch in `run_loop.rs`

Open `crates/agentos-kernel/src/run_loop.rs`. Add a dispatch arm for `GetTaskResult` in the command match block:

```rust
KernelCommand::GetTaskResult { task_id } => {
    kernel.cmd_get_task_result(task_id).await
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-bus/src/message.rs` | Add `TaskStarted` to `KernelResponse`; add `GetTaskResult` to `KernelCommand` |
| `crates/agentos-kernel/src/scheduler.rs` | Add `task_results` field, `TaskResultSummary` struct, `store_task_result()`, `get_task_result()` |
| `crates/agentos-kernel/src/commands/task.rs` | Refactor `cmd_run_task` for async spawning; add `cmd_get_task_result` |
| `crates/agentos-kernel/src/run_loop.rs` | Add dispatch arm for `GetTaskResult` |

## Prerequisites

None -- this is the first phase.

## Test Plan

- **Unit test `test_task_result_storage`:** Store a `TaskResultSummary` in the scheduler, retrieve it by `TaskID`, assert all fields match.
- **Unit test `test_task_result_not_found`:** Query a non-existent `TaskID`, assert `None` returned.
- **Unit test `test_cmd_get_task_result_pending`:** Create a task in `Queued` state with no result stored; call `cmd_get_task_result`; assert response contains `"state": "Queued"`.
- **Integration test `test_async_task_completes`:** Submit a task with `blocking: false`, poll `GetTaskResult` in a loop with 100ms interval, assert terminal state reached within 30s.

## Verification

```bash
cargo build -p agentos-kernel -p agentos-bus
cargo test -p agentos-kernel -- --nocapture
cargo test -p agentos-bus -- --nocapture
cargo clippy -p agentos-kernel -p agentos-bus -- -D warnings
```
