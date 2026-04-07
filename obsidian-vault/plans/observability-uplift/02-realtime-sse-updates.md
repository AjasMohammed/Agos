---
title: "Phase 2: Real-Time SSE Updates"
tags:
  - web
  - kernel
  - plan
date: 2026-04-03
status: planned
effort: 2d
priority: high
---

# Phase 2: Real-Time SSE Updates

> Add a `GET /api/tasks/stream` SSE endpoint that pushes task state changes, sub-agent spawns, and completion events to the browser in real time, enabling the DAG view to update live without polling.

---

## Why This Phase

The DAG view from Phase 1 is static -- it renders the task tree at the moment the page loads. Multi-agent coordination produces rapid state changes as sub-agents are spawned, execute, and complete. Without real-time updates, the operator must manually refresh to see progress.

The existing web UI already has SSE infrastructure: `AppState.notification_tx: broadcast::Sender<NotificationSsePayload>` pushes user notifications to the browser. This phase adds a parallel channel for task events.

HTMX has built-in SSE support via `hx-ext="sse"` and `hx-sse="connect:/api/tasks/stream"`, which makes integrating real-time updates into the DAG template straightforward.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Task event streaming | None | `GET /api/tasks/stream` SSE endpoint pushing `TaskEvent` payloads |
| DAG view updates | Static; requires page refresh | HTMX SSE swaps update individual task nodes in-place |
| Broadcast channel | `notification_tx` for user notifications only | New `task_event_tx: broadcast::Sender<TaskEvent>` on `AppState` |
| Event types streamed | None | `TaskStateChanged`, `SubAgentSpawned`, `TaskCompleted`, `TaskFailed` |

## What to Do

### 1. Define `TaskEvent`

Create a shared event type. Add to `crates/agentos-web/src/handlers/dag.rs` (or a new `crates/agentos-web/src/task_events.rs`):

```rust
use agentos_types::{AgentID, TaskID, TaskState};
use serde::{Deserialize, Serialize};

/// Event pushed over SSE to browser subscribers for real-time task monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub event_type: TaskEventType,
    pub task_id: TaskID,
    pub agent_id: Option<AgentID>,
    pub agent_name: Option<String>,
    pub parent_task_id: Option<TaskID>,
    pub state: Option<TaskState>,
    pub spawn_depth: Option<u8>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskEventType {
    StateChanged,
    SubAgentSpawned,
    Completed,
    Failed,
    Cancelled,
}
```

### 2. Add broadcast channel to `AppState`

Open `crates/agentos-web/src/state.rs`. Add:

```rust
/// Broadcast channel for real-time task event push to browser SSE subscribers.
pub task_event_tx: broadcast::Sender<TaskEvent>,
```

Initialize in the `AppState` constructor:

```rust
let (task_event_tx, _) = tokio::sync::broadcast::channel(512);
```

### 3. Emit task events from the kernel

The `completion_tx` from [[Event-Driven Completion Plan]] carries `TaskCompletionEvent`. Bridge it to the web `task_event_tx` by adding a listener in the kernel's run loop or in the web server startup:

Open `crates/agentos-web/src/server.rs`. After creating `AppState`, spawn a bridge task:

```rust
// Bridge kernel completion events to web SSE task events.
let task_event_tx_clone = state.task_event_tx.clone();
let mut completion_rx = state.kernel.completion_tx.subscribe();
tokio::spawn(async move {
    loop {
        match completion_rx.recv().await {
            Ok(event) => {
                let task_event = TaskEvent {
                    event_type: match event.final_state {
                        TaskState::Complete => TaskEventType::Completed,
                        TaskState::Failed => TaskEventType::Failed,
                        TaskState::Cancelled => TaskEventType::Cancelled,
                        _ => TaskEventType::StateChanged,
                    },
                    task_id: event.task_id,
                    agent_id: Some(event.agent_id),
                    agent_name: None, // resolved by the DAG handler
                    parent_task_id: event.parent_task_id,
                    state: Some(event.final_state),
                    spawn_depth: None,
                    timestamp: event.completed_at,
                };
                let _ = task_event_tx_clone.send(task_event);
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(lagged = n, "task event bridge: lagged behind");
            }
            Err(_) => break, // channel closed
        }
    }
});
```

Additionally, emit `SubAgentSpawned` events from `cmd_spawn_sub_agent` by sending to a kernel-side broadcast channel that the bridge also subscribes to. Alternatively, have the bridge listen on the audit log's event stream.

### 4. Create the SSE handler

Add to `crates/agentos-web/src/handlers/dag.rs`:

```rust
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;
use std::convert::Infallible;

/// SSE endpoint for real-time task events.
pub async fn task_event_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.task_event_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let json = serde_json::to_string(&event).unwrap_or_default();
                    let sse_event = Event::default()
                        .event(format!("{:?}", event.event_type).to_lowercase())
                        .data(json);
                    yield Ok(sse_event);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Skip lagged events; the client will catch up.
                    continue;
                }
                Err(_) => break, // channel closed
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

### 5. Register the SSE route

Open `crates/agentos-web/src/router.rs`. Add:

```rust
.route("/api/tasks/stream", axum::routing::get(dag::task_event_stream))
```

### 6. Update the DAG template for HTMX SSE

Open `crates/agentos-web/templates/dag.html`. Add HTMX SSE integration:

```html
<div id="dag-tree"
     hx-ext="sse"
     sse-connect="/api/tasks/stream"
     sse-swap="completed,failed,cancelled,statechanged"
     hx-swap="innerHTML"
     hx-target="#dag-tree"
     hx-get="/tasks/dag?partial=true"
     hx-trigger="sse:completed, sse:failed, sse:cancelled, sse:statechanged">
    {{ tree_html|safe }}
</div>
```

This tells HTMX to re-fetch the DAG partial whenever a task event arrives, swapping the tree in place. The `partial=true` query param tells the handler to return just the tree fragment, not the full page.

### 7. Add partial rendering support to DAG handler

In the `dag_view` handler, check for `partial=true` query parameter:

```rust
pub async fn dag_view(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    // ... build tree_html ...
    
    if params.get("partial").map(|v| v == "true").unwrap_or(false) {
        return Html(tree_html);
    }
    
    // ... render full template ...
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/handlers/dag.rs` | Add `TaskEvent`, `TaskEventType`, `task_event_stream` SSE handler, partial rendering |
| `crates/agentos-web/src/state.rs` | Add `task_event_tx` broadcast channel |
| `crates/agentos-web/src/server.rs` | Spawn bridge task from `completion_tx` to `task_event_tx` |
| `crates/agentos-web/src/router.rs` | Add `/api/tasks/stream` SSE route |
| `crates/agentos-web/templates/dag.html` | Add HTMX SSE attributes for live updates |

## Prerequisites

[[01-task-dag-view]] must be complete first -- this phase adds live updates to the DAG view.

Also depends on [[02-completion-event-emission]] from [[Event-Driven Completion Plan]] for the `completion_tx` broadcast channel.

## Test Plan

- **Unit test `test_task_event_serialization`:** Serialize a `TaskEvent` to JSON, deserialize it, assert round-trip fidelity.
- **Unit test `test_task_event_bridge`:** Create a `completion_tx`, subscribe a `task_event_tx`, send a `TaskCompletionEvent`. Assert the bridge converts it to a `TaskEvent` on the other side.
- **Integration test `test_sse_endpoint_sends_events`:** Start the web server, connect to `/api/tasks/stream` via an HTTP client, emit a completion event, assert the SSE stream delivers a JSON event within 5 seconds.
- **Unit test `test_partial_dag_rendering`:** Call `dag_view` with `partial=true`. Assert the response does not contain `<html>` or `{% extends %}` -- just the tree fragment.

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web -- --nocapture
cargo clippy -p agentos-web -- -D warnings
cargo fmt --all -- --check
```
