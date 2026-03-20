---
title: Add Task Assignment from Chat
tags:
  - web
  - kernel
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 1.5d
priority: medium
---

# Add Task Assignment from Chat

> Allow users to create formal kernel tasks from the chat interface using a `/task` command prefix, with inline task status tracking and links to the task detail page.

---

## Why This Subtask

Chat and tasks serve different purposes. Chat is conversational and ephemeral -- good for questions and exploration. Tasks are tracked, audited, cost-budgeted, and persisted by the kernel scheduler -- good for actual work. Users need a seamless way to escalate from "talking about a file" to "have the agent actually process it" without leaving the chat.

This subtask adds:
1. A `/task <prompt>` command prefix that creates a real `AgentTask` from chat.
2. A `task` role in `chat_messages` that records the task assignment.
3. An inline status badge that polls the task state via HTMX.
4. A link to the full task detail page (`/tasks/{id}`).

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Task creation from chat | Not possible | `/task <prompt>` command in the chat input |
| Chat message for task | Does not exist | `role='task'` with `task_id` column |
| Task status in chat | Not shown | HTMX-polled badge that updates every 5s |
| Schema version | 1 (from subtask 28-02) | 2 (adds `task_id`, expands role CHECK to include `'task'`) |
| Task creation API | Only via CLI/bus `SubmitTask` | New `Kernel::create_chat_task()` method (simplified wrapper) |

---

## What to Do

### Step 1: Schema migration v2

Open `crates/agentos-web/src/chat_store.rs`. Add after the version 1 migration:

```rust
if version < 2 {
    conn.execute_batch(
        "ALTER TABLE chat_messages RENAME TO _chat_messages_v1;
         CREATE TABLE chat_messages (
             id               INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id       TEXT    NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
             role             TEXT    NOT NULL CHECK(role IN ('user', 'assistant', 'tool', 'task')),
             content          TEXT    NOT NULL,
             tool_name        TEXT,
             tool_duration_ms INTEGER,
             task_id          TEXT,
             created_at       TEXT    NOT NULL
         );
         INSERT INTO chat_messages (id, session_id, role, content, tool_name, tool_duration_ms, created_at)
             SELECT id, session_id, role, content, tool_name, tool_duration_ms, created_at
             FROM _chat_messages_v1;
         DROP TABLE _chat_messages_v1;
         CREATE INDEX IF NOT EXISTS idx_chat_msg_session ON chat_messages(session_id, id);
         UPDATE chat_store_version SET version = 2 WHERE rowid = 1;"
    )?;
}
```

Update `ChatMessage` struct:

```rust
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub created_at: String,
    pub tool_name: Option<String>,
    pub tool_duration_ms: Option<u64>,
    pub task_id: Option<String>,
}
```

Update `get_messages()` to select `task_id`:

```rust
"SELECT role, content, created_at, tool_name, tool_duration_ms, task_id
 FROM chat_messages ..."
// ... row mapping:
task_id: row.get(5)?,
```

### Step 2: Add `add_task_message()` to ChatStore

```rust
/// Record a task assignment in the chat timeline.
pub fn add_task_message(
    &self,
    session_id: &str,
    task_id: &str,
    prompt: &str,
) -> Result<(), rusqlite::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let content = serde_json::json!({
        "prompt": prompt,
        "status": "pending",
    })
    .to_string();
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO chat_messages (session_id, role, content, task_id, created_at)
         VALUES (?1, 'task', ?2, ?3, ?4)",
        rusqlite::params![session_id, content, task_id, now],
    )?;
    tx.execute(
        "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now, session_id],
    )?;
    tx.commit()?;
    Ok(())
}
```

### Step 3: Add task creation helper to Kernel

The kernel's task creation path currently goes through the `SubmitTask` bus command, which requires building an `IntentMessage`. For chat-originated tasks, add a convenience method.

Check `crates/agentos-kernel/src/scheduler.rs` for the task creation API. The scheduler's `submit_task()` or equivalent method needs to be called. The agent's capability token can be the default profile from `ProfileManager`.

In `crates/agentos-kernel/src/kernel.rs`, add:

```rust
/// Create a task from the chat interface. Uses the agent's default capability profile.
/// Returns the TaskID on success.
pub async fn create_chat_task(
    &self,
    agent_name: &str,
    prompt: &str,
) -> Result<TaskID, String> {
    let agent = {
        let registry = self.agent_registry.read().await;
        registry
            .get_by_name(agent_name)
            .cloned()
            .ok_or_else(|| format!("Agent '{}' not found", agent_name))?
    };

    if agent.status == AgentStatus::Offline {
        return Err(format!("Agent '{}' is offline", agent_name));
    }

    // Build a default capability token for the task.
    let capability_token = self
        .capability_engine
        .mint_token(agent.id, &[], chrono::Duration::hours(1))
        .map_err(|e| format!("Failed to mint capability token: {}", e))?;

    let task = AgentTask::new(agent.id, prompt.to_string(), capability_token);
    let task_id = task.id;
    self.scheduler.submit(task).await;
    Ok(task_id)
}
```

The exact API depends on how `TaskScheduler::submit()` works. Consult `crates/agentos-kernel/src/scheduler.rs` for the correct signature and adjust accordingly.

### Step 4: Detect `/task` in the send handler

Open `crates/agentos-web/src/handlers/chat.rs`. In the `send()` handler, add a check before the inference path:

```rust
// Check for /task command.
if message.starts_with("/task ") {
    let prompt = message.strip_prefix("/task ").unwrap().trim().to_string();
    if prompt.is_empty() {
        return (StatusCode::BAD_REQUEST, "Task prompt cannot be empty").into_response();
    }

    // Save the user message (the /task command).
    // ... (existing user message save code) ...

    // Create the kernel task.
    let task_id = match state.kernel.create_chat_task(&session.agent_name, &prompt).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Failed to create task from chat: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to create task").into_response();
        }
    };

    // Save the task reference in chat.
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    let tid = task_id.to_string();
    let p = prompt.clone();
    let _ = tokio::task::spawn_blocking(move || store.add_task_message(&sid, &tid, &p)).await;

    // Return HTMX partial with the task activity row.
    let short_id = &task_id.to_string()[..8];
    let html = format!(
        r#"<div class="chat-activity" style="width:100%; max-width:80%;">
            <span class="chat-activity-icon" aria-hidden="true">&#9654;</span>
            <span class="chat-activity-label">
                Task assigned: <a href="/tasks/{tid}">{short_id}</a> &mdash; {prompt_preview}
            </span>
            <span hx-get="/tasks/{tid}/status-badge"
                  hx-trigger="every 5s"
                  hx-swap="innerHTML"
                  class="chat-activity-ts">pending</span>
        </div>"#,
        tid = task_id,
        short_id = short_id,
        prompt_preview = if prompt.len() > 60 {
            format!("{}...", &prompt[..60])
        } else {
            prompt.clone()
        },
    );
    return axum::response::Html(html).into_response();
}
```

### Step 5: Add task status badge endpoint

Open `crates/agentos-web/src/handlers/tasks.rs`. Add:

```rust
/// GET /tasks/{id}/status-badge -- returns a small HTML badge for inline display.
pub async fn status_badge(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let task_id = match id.parse::<agentos_types::TaskID>() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid task ID").into_response(),
    };

    let tasks = state.kernel.scheduler.list_tasks().await;
    let task = tasks.iter().find(|t| t.id == task_id);

    let (label, class) = match task.map(|t| &t.state) {
        Some(agentos_types::TaskState::Running) => ("running", ""),
        Some(agentos_types::TaskState::Complete) => ("complete", "style=\"color:#28a745\""),
        Some(agentos_types::TaskState::Failed) => ("failed", "style=\"color:#dc3545\""),
        Some(agentos_types::TaskState::Waiting) => ("waiting", ""),
        Some(agentos_types::TaskState::Cancelled) => ("cancelled", "style=\"color:#6c757d\""),
        _ => ("pending", ""),
    };

    axum::response::Html(format!("<small {}>{}</small>", class, label)).into_response()
}
```

### Step 6: Register the status badge route

Open `crates/agentos-web/src/router.rs`. Add:

```rust
.route("/tasks/{id}/status-badge", axum::routing::get(tasks::status_badge))
```

### Step 7: Render task messages in `partials/chat_message.html`

Add a case for `role == "task"`:

```html
{% elif msg.role == "task" %}
<div class="chat-row">
    <div class="chat-activity" style="width:100%; max-width:80%;">
        <span class="chat-activity-icon" aria-hidden="true">&#9654;</span>
        <span class="chat-activity-label">
            Task: <a href="/tasks/{{ msg.task_id }}">{{ msg.task_id[:8] }}</a>
        </span>
        <span hx-get="/tasks/{{ msg.task_id }}/status-badge"
              hx-trigger="every 5s"
              hx-swap="innerHTML"
              class="chat-activity-ts">...</span>
    </div>
</div>
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/chat_store.rs` | Migration v2; add `task_id` to `ChatMessage`; add `add_task_message()` |
| `crates/agentos-kernel/src/kernel.rs` | Add `create_chat_task()` |
| `crates/agentos-web/src/handlers/chat.rs` | Detect `/task` prefix in `send()`; create task; return activity partial |
| `crates/agentos-web/src/handlers/tasks.rs` | Add `status_badge()` |
| `crates/agentos-web/src/router.rs` | Add `/tasks/{id}/status-badge` route |
| `crates/agentos-web/src/templates/partials/chat_message.html` | Add `task` role rendering |

---

## Prerequisites

- [[28-01-Add Chat Tool Execution Loop to Kernel]] -- the kernel infrastructure must be in place.
- [[28-04-Rewrite Chat Conversation Template with HTMX]] -- the template and partial must exist.

---

## Test Plan

- `cargo build -p agentos-kernel -p agentos-web` must compile.
- Add test `test_chat_store_task_message_roundtrip`:
  - Create session, call `add_task_message(session_id, "task-uuid", "summarize file")`.
  - Call `get_messages(session_id)`.
  - Verify message with `role == "task"` exists and `task_id` is populated.
- Add test `test_task_status_badge_endpoint`:
  - Create a task in the scheduler. GET `/tasks/{id}/status-badge`. Verify 200 with status label.
- Manual test: In chat, type `/task list all files in /data`. Verify a task activity row appears with a link. Verify the status badge updates from "pending" to "running" to "complete".

---

## Verification

```bash
cargo build -p agentos-kernel -p agentos-web
cargo test -p agentos-web -- chat --nocapture
cargo test -p agentos-web -- task --nocapture
cargo clippy -p agentos-kernel -p agentos-web -- -D warnings
```
