---
title: Chat Task Assignment from Chat
tags:
  - web
  - kernel
  - v3
  - plan
date: 2026-03-18
status: planned
effort: 1.5d
priority: medium
---

# Phase 06 -- Task Assignment from Chat

> Allow users to assign formal kernel tasks to agents directly from the chat interface, with inline status tracking of task progress.

---

## Why This Phase

Chat is conversational and ephemeral. Tasks are tracked, audited, and persisted by the kernel scheduler. Users need a way to escalate from "chatting about a file" to "assign the agent to actually process it" without leaving the chat interface. This phase:

1. Detects when the user's message is a task assignment (e.g., starts with `/task` or "assign:", or uses a dedicated button).
2. Creates a real `AgentTask` in the kernel scheduler.
3. Shows the task's status inline in the chat conversation.
4. Links to the full task detail page for deeper inspection.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Task creation from chat | Not possible | `/task <prompt>` command or "Assign Task" button |
| Task status in chat | Not shown | Inline status badge that updates via SSE |
| Chat-to-task linking | No relationship | `chat_messages` can store `task_id` for task-type messages |
| Task detail link | N/A | Clickable link from chat activity to `/tasks/{id}` |

---

## What to Do

### Step 1: Add task_id column to chat_messages

In `crates/agentos-web/src/chat_store.rs`, add a schema migration (version 2):

```rust
if version < 2 {
    conn.execute_batch(
        "ALTER TABLE chat_messages ADD COLUMN task_id TEXT;
         UPDATE chat_store_version SET version = 2;"
    )?;
}
```

Update `ChatMessage` struct to include `pub task_id: Option<String>`.

### Step 2: Add `add_task_message()` to ChatStore

```rust
/// Record a task assignment in the chat history.
pub fn add_task_message(
    &self,
    session_id: &str,
    task_id: &str,
    prompt: &str,
) -> Result<(), rusqlite::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let content = serde_json::json!({
        "task_id": task_id,
        "prompt": prompt,
        "status": "pending",
    }).to_string();
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO chat_messages (session_id, role, content, task_id, created_at)
         VALUES (?1, 'task', ?2, ?3, ?4)",
        params![session_id, content, task_id, now],
    )?;
    tx.execute(
        "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
        params![now, session_id],
    )?;
    tx.commit()?;
    Ok(())
}
```

Note: This also requires updating the role CHECK constraint to include `'task'` (migration version 2).

### Step 3: Detect task assignment in the send handler

In `crates/agentos-web/src/handlers/chat.rs`, in the `send()` handler, check if the message starts with `/task `:

```rust
if message.starts_with("/task ") {
    let prompt = message.strip_prefix("/task ").unwrap().trim();
    // Look up the agent and create a real task via the kernel
    let agent_id = {
        let registry = state.kernel.agent_registry.read().await;
        registry.get_by_name(&session.agent_name).map(|a| a.id)
    };
    if let Some(agent_id) = agent_id {
        let task_id = state.kernel.scheduler.create_task(agent_id, prompt).await;
        // Save task reference in chat
        let store = Arc::clone(&state.chat_store);
        let sid = session_id.clone();
        let tid = task_id.to_string();
        let p = prompt.to_string();
        let _ = tokio::task::spawn_blocking(move || store.add_task_message(&sid, &tid, &p)).await;
        // Return HTMX partial with task status widget
        // ...
    }
    return /* task assignment response */;
}
```

The exact kernel API for creating tasks programmatically depends on the existing `TaskScheduler` interface. Consult `scheduler.rs` for the correct method signature.

### Step 4: Add task status SSE event

Add a `chat-task-status` event to the chat SSE stream that polls the task's state and emits updates:

```rust
ChatStreamEvent::TaskStatus {
    task_id: String,
    state: String,  // "pending", "running", "complete", "failed"
    result_preview: Option<String>,
}
```

### Step 5: Render task messages in the conversation template

In `partials/chat_message.html`, add a case for `role == "task"`:

```html
{% elif msg.role == "task" %}
<div class="chat-row">
    <div class="chat-activity chat-activity-task">
        <span class="chat-activity-icon" aria-hidden="true">&#9654;</span>
        <span class="chat-activity-label">
            Task assigned: <a href="/tasks/{{ msg.task_id }}">{{ msg.task_id[:8] }}</a>
        </span>
        <span class="chat-activity-ts"
              hx-get="/tasks/{{ msg.task_id }}/status-badge"
              hx-trigger="every 5s"
              hx-swap="innerHTML">
            pending
        </span>
    </div>
</div>
{% endif %}
```

### Step 6: Add task status badge endpoint

In `crates/agentos-web/src/handlers/tasks.rs`, add:

```rust
/// GET /tasks/{id}/status-badge -- returns a small HTML badge for inline display.
pub async fn status_badge(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    // Look up task state, return a small badge HTML
}
```

Register in router:
```rust
.route("/tasks/{id}/status-badge", axum::routing::get(tasks::status_badge))
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/chat_store.rs` | Migration v2 (add `task_id`, expand role CHECK); `add_task_message()` |
| `crates/agentos-web/src/handlers/chat.rs` | Detect `/task` prefix in `send()`; create kernel task |
| `crates/agentos-web/src/handlers/tasks.rs` | Add `status_badge()` endpoint |
| `crates/agentos-web/src/router.rs` | Add `/tasks/{id}/status-badge` route |
| `crates/agentos-web/src/templates/partials/chat_message.html` | Add `task` role rendering |

---

## Dependencies

- [[01-chat-tool-execution-loop]] -- provides the kernel chat infrastructure.
- [[04-chat-conversation-template-htmx]] -- provides the HTMX-based conversation template and `chat_message.html` partial.

---

## Test Plan

- `cargo test -p agentos-web` must pass.
- Add test `test_task_assignment_from_chat`: Send a message starting with `/task`, verify a task is created in the scheduler and a task message is recorded in the chat store.
- Add test `test_chat_store_task_message`: Create a task message via `add_task_message()`, verify it loads with correct `task_id`.
- Manual test: In chat, type `/task summarize the audit log`, verify a task activity row appears with a link to the task detail page.
- Manual test: Verify the task status badge updates as the task progresses.

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web -- chat --nocapture
cargo clippy -p agentos-web -- -D warnings
```
