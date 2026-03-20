---
title: Chat Agent Selection and History
tags:
  - web
  - v3
  - plan
date: 2026-03-18
status: planned
effort: 1d
priority: medium
---

# Phase 05 -- Chat Agent Selection and History

> Add agent switching within a conversation, session deletion, session search/filter, and message history pagination to improve the chat UX.

---

## Why This Phase

After Phases 01-04, the core chat experience works (tool execution + streaming). This phase addresses usability gaps:

1. The user must navigate back to `/chat` to start a new conversation -- there is no way to select a different agent from within a conversation.
2. Sessions cannot be deleted.
3. With many sessions, there is no search or filter.
4. Messages are loaded with a hard limit of 200 (`LIMIT 200` in `get_messages()`), but there is no pagination UI.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Agent selection | Only on `/chat` new session form | Dropdown in conversation sidebar |
| Session deletion | Not possible | DELETE button per session + DELETE endpoint |
| Session search | Not possible | Text filter on session list (client-side) |
| Message pagination | Hard limit 200, no "load more" | "Load earlier messages" button at top |
| Active session highlight | None | Current session highlighted in sidebar |

---

## What to Do

### Step 1: Add session deletion endpoint

In `crates/agentos-web/src/handlers/chat.rs`, add:

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

### Step 2: Add `delete_session()` to ChatStore

In `crates/agentos-web/src/chat_store.rs`:

```rust
/// Delete a session and all its messages. The ON DELETE CASCADE foreign key
/// on chat_messages handles message cleanup automatically.
pub fn delete_session(&self, session_id: &str) -> Result<(), rusqlite::Error> {
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    conn.execute("DELETE FROM chat_sessions WHERE id = ?1", params![session_id])?;
    Ok(())
}
```

### Step 3: Add message pagination to ChatStore

In `crates/agentos-web/src/chat_store.rs`, add a paginated variant:

```rust
/// Return messages older than the given message ID, for "load more" pagination.
pub fn get_messages_before(
    &self,
    session_id: &str,
    before_id: i64,
    limit: usize,
) -> Result<Vec<ChatMessage>, rusqlite::Error> {
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    let mut stmt = conn.prepare(
        "SELECT role, content, created_at, tool_name, tool_duration_ms, id
         FROM chat_messages
         WHERE session_id = ?1 AND id < ?2
         ORDER BY id DESC
         LIMIT ?3",
    )?;
    let mut rows: Vec<ChatMessage> = stmt
        .query_map(params![session_id, before_id, limit as i64], |row| {
            Ok(ChatMessage {
                role: row.get(0)?,
                content: row.get(1)?,
                created_at: row.get(2)?,
                tool_name: row.get(3)?,
                tool_duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
            })
        })?
        .collect::<Result<_, _>>()?;
    rows.reverse();
    Ok(rows)
}
```

### Step 4: Register the DELETE route

In `crates/agentos-web/src/router.rs`, update the existing chat session route:

```rust
.route("/chat/{session_id}", axum::routing::get(chat::conversation).delete(chat::delete_session))
```

### Step 5: Update `chat.html` template

Add a search filter input and delete buttons to the session list:

- Add `<input type="search" placeholder="Filter sessions..." x-model="filter">` above the session list.
- Use Alpine.js `x-show` to filter sessions client-side by agent name or preview text.
- Add a delete button per session: `<button hx-delete="/chat/{{s.id}}" hx-confirm="Delete this session?" hx-target="closest .chat-thread-item" hx-swap="outerHTML">`.

### Step 6: Update `chat_conversation.html` sidebar

Add the session list as a sidebar within the conversation view so the user can switch sessions without navigating away. Include the agent selector dropdown.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/handlers/chat.rs` | Add `delete_session()` handler |
| `crates/agentos-web/src/chat_store.rs` | Add `delete_session()`, `get_messages_before()` |
| `crates/agentos-web/src/router.rs` | Add DELETE method to `/chat/{session_id}` |
| `crates/agentos-web/src/templates/chat.html` | Add search filter, delete buttons |
| `crates/agentos-web/src/templates/chat_conversation.html` | Add session sidebar, agent selector |

---

## Dependencies

[[04-chat-conversation-template-htmx]] must be complete (the conversation template is rewritten there).

---

## Test Plan

- `cargo test -p agentos-web` must pass.
- Add test `test_chat_store_delete_session`: Create session, add messages, delete session, verify `get_session()` returns `None`.
- Add test `test_chat_store_messages_before`: Create 10 messages, request 5 before id=8, verify correct 5 returned in order.
- Manual test: Delete a session, verify it disappears from the list.
- Manual test: Type in the search box, verify sessions filter.
- Manual test: Click "Load earlier messages", verify older messages appear.

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web -- chat --nocapture
cargo clippy -p agentos-web -- -D warnings
```
