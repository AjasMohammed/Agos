---
title: Add Chat Session Management Features
tags:
  - web
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 1d
priority: medium
---

# Add Chat Session Management Features

> Add session deletion, client-side session search/filter, message pagination ("load earlier"), and active session highlighting to the chat UI.

---

## Why This Subtask

The core chat experience (tool execution + streaming) is functional after subtasks 28-01 through 28-04. This subtask addresses UX gaps:

- Sessions cannot be deleted, so the list grows indefinitely.
- There is no way to search or filter sessions when the list is long.
- Messages are limited to 200 with no way to load older messages.
- The active session is not highlighted in the session list.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Session deletion | Not possible | DELETE button per session, HTMX-powered removal |
| Session search | Not possible | Client-side text filter using Alpine.js `x-model` |
| Message pagination | `LIMIT 200` with no UI to load more | "Load earlier messages" button using HTMX `hx-get` |
| Active session highlight | None | CSS class `chat-thread-active` on current session |
| `ChatStore::delete_session()` | Does not exist | New method using `DELETE FROM chat_sessions WHERE id = ?1` |
| `ChatStore::get_messages_before()` | Does not exist | New method with `id < ?` and `LIMIT` parameters |

---

## What to Do

### Step 1: Add `delete_session()` to ChatStore

Open `crates/agentos-web/src/chat_store.rs`. Add:

```rust
/// Delete a session and all its messages. The `ON DELETE CASCADE` foreign key
/// on chat_messages handles message cleanup automatically.
pub fn delete_session(&self, session_id: &str) -> Result<(), rusqlite::Error> {
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    conn.execute(
        "DELETE FROM chat_sessions WHERE id = ?1",
        rusqlite::params![session_id],
    )?;
    Ok(())
}
```

### Step 2: Add `get_messages_before()` to ChatStore

```rust
/// Return up to `limit` messages with id < `before_id`, in chronological order.
/// Used for "load earlier messages" pagination.
pub fn get_messages_before(
    &self,
    session_id: &str,
    before_id: i64,
    limit: usize,
) -> Result<(Vec<ChatMessage>, bool), rusqlite::Error> {
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    let mut stmt = conn.prepare(
        "SELECT role, content, created_at, tool_name, tool_duration_ms, id
         FROM chat_messages
         WHERE session_id = ?1 AND id < ?2
         ORDER BY id DESC
         LIMIT ?3",
    )?;
    let limit_plus_one = (limit + 1) as i64;
    let mut rows: Vec<(ChatMessage, i64)> = stmt
        .query_map(rusqlite::params![session_id, before_id, limit_plus_one], |row| {
            Ok((
                ChatMessage {
                    role: row.get(0)?,
                    content: row.get(1)?,
                    created_at: row.get(2)?,
                    tool_name: row.get(3)?,
                    tool_duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                },
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    rows.reverse();
    Ok((rows.into_iter().map(|(m, _)| m).collect(), has_more))
}
```

Also update `get_messages()` to return message IDs so the template can use the first ID as the `before_id` for pagination. Add a `message_id` field to `ChatMessage`, or return it as a separate value in the template context.

### Step 3: Add the delete handler

Open `crates/agentos-web/src/handlers/chat.rs`. Add:

```rust
/// DELETE /chat/{session_id} -- delete a session and all its messages.
pub async fn delete_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    if uuid::Uuid::parse_str(&session_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid session ID").into_response();
    }
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    match tokio::task::spawn_blocking(move || store.delete_session(&sid)).await {
        Ok(Ok(())) => StatusCode::OK.into_response(),
        Ok(Err(e)) => {
            tracing::error!("Failed to delete chat session: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Delete failed").into_response()
        }
        Err(e) => {
            tracing::error!("spawn_blocking panicked: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}
```

### Step 4: Register the DELETE route

Open `crates/agentos-web/src/router.rs`. Update line 137:

```rust
.route("/chat/{session_id}", axum::routing::get(chat::conversation).delete(chat::delete_session))
```

### Step 5: Update `chat.html` template

Add Alpine.js search filter and HTMX delete buttons:

In the session list sidebar, wrap the list in an Alpine.js component:

```html
<aside class="chat-sidebar" x-data="{ filter: '' }">
    <div class="chat-sidebar-header">
        <strong>Sessions</strong>
        <input type="search" placeholder="Filter..." x-model="filter"
               style="margin:0.3rem 0 0; padding:0.3rem 0.5rem; font-size:0.8rem;">
    </div>
    <nav class="chat-thread-list">
        {% for s in sessions %}
        <a href="/chat/{{ s.id }}"
           class="chat-thread-item {% if s.id == active_session_id %}chat-thread-active{% endif %}"
           x-show="filter === '' || '{{ s.agent_name }} {{ s.preview }}'.toLowerCase().includes(filter.toLowerCase())">
            <div class="chat-thread-agent">{{ s.agent_name }}</div>
            {% if s.preview %}
            <div class="chat-thread-preview muted">{{ s.preview }}</div>
            {% endif %}
            <div class="chat-thread-meta">
                <small class="muted">{{ s.updated_at }}</small>
                <button class="btn-icon-sm"
                        hx-delete="/chat/{{ s.id }}"
                        hx-confirm="Delete this session and all messages?"
                        hx-target="closest .chat-thread-item"
                        hx-swap="outerHTML"
                        onclick="event.preventDefault(); event.stopPropagation();"
                        title="Delete session"
                        aria-label="Delete session">&#215;</button>
            </div>
        </a>
        {% endfor %}
    </nav>
</aside>
```

### Step 6: Add CSS for active session and delete button

Add to `crates/agentos-web/static/css/app.css`:

```css
.chat-thread-active {
    background: var(--pico-primary-focus);
    border-left: 3px solid var(--pico-primary);
}

.btn-icon-sm {
    background: none;
    border: none;
    cursor: pointer;
    padding: 0 0.3rem;
    font-size: 0.9rem;
    color: var(--pico-muted-color);
    opacity: 0;
    transition: opacity 0.15s;
}
.chat-thread-item:hover .btn-icon-sm { opacity: 1; }
.btn-icon-sm:hover { color: var(--pico-del-color, #dc3545); }
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/chat_store.rs` | Add `delete_session()`, `get_messages_before()` |
| `crates/agentos-web/src/handlers/chat.rs` | Add `delete_session()` handler |
| `crates/agentos-web/src/router.rs` | Add DELETE method to `/chat/{session_id}` |
| `crates/agentos-web/src/templates/chat.html` | Add search filter, delete buttons, active highlight |
| `crates/agentos-web/static/css/app.css` | Add `.chat-thread-active`, `.btn-icon-sm` styles |

---

## Prerequisites

[[28-04-Rewrite Chat Conversation Template with HTMX]] must be complete (the conversation template is finalized there).

---

## Test Plan

- `cargo test -p agentos-web` must pass.
- Add test `test_chat_store_delete_session`:
  - Create a session with 3 messages.
  - Call `delete_session()`.
  - Verify `get_session()` returns `None`.
  - Verify `get_messages()` returns empty (cascade delete).
- Add test `test_chat_store_get_messages_before`:
  - Create a session with 10 messages.
  - Call `get_messages_before(session_id, 8, 3)`.
  - Verify 3 messages returned, all with id < 8, in chronological order.
  - Verify `has_more` is `true` (there are more messages before).
- Manual test: Click the delete button on a session, confirm, verify it disappears.
- Manual test: Type in the search box, verify sessions filter by agent name and preview text.

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web -- chat_store --nocapture
cargo clippy -p agentos-web -- -D warnings
```
