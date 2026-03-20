---
title: Extend ChatStore Schema for Tool Metadata
tags:
  - web
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 0.5d
priority: high
---

# Extend ChatStore Schema for Tool Metadata

> Add tool call metadata columns to the `chat_messages` SQLite table and a new `add_tool_calls()` method to `ChatStore`, so tool activity is persisted alongside chat messages and visible when revisiting a conversation.

---

## Why This Subtask

After subtask 28-01, the kernel returns `ChatInferenceResult` containing a `tool_calls: Vec<ChatToolCallRecord>` field. But the `ChatStore` schema in `crates/agentos-web/src/chat_store.rs` only allows `role IN ('user', 'assistant')`. Tool call records cannot be persisted.

Without this subtask, tool calls executed during chat are lost when the page is refreshed. The conversation template (subtask 28-04) needs tool records to render activity indicators, and the LLM needs tool results in the history to maintain conversation coherence on follow-up messages.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `chat_messages.role` CHECK | `CHECK(role IN ('user', 'assistant'))` | `CHECK(role IN ('user', 'assistant', 'tool'))` |
| `chat_messages.tool_name` | Does not exist | `TEXT` column, nullable |
| `chat_messages.tool_duration_ms` | Does not exist | `INTEGER` column, nullable |
| Schema versioning | No version table | `chat_store_version` table with migration tracking |
| `ChatMessage` struct | `{ role, content, created_at }` | Add `tool_name: Option<String>`, `tool_duration_ms: Option<u64>` |
| `ChatStore::add_tool_calls()` | Does not exist | Batch-insert tool records for a session |
| `ChatStore::get_messages()` | Selects `role, content, created_at` | Selects `role, content, created_at, tool_name, tool_duration_ms` |

---

## What to Do

1. Open `crates/agentos-web/src/chat_store.rs`.

2. Add a version table and migration logic at the end of the `open()` method, after the existing `CREATE TABLE IF NOT EXISTS` block (line 48). SQLite does not support `ALTER TABLE ... ALTER CHECK CONSTRAINT`, so the migration recreates the table:

```rust
// Schema versioning for forward migrations.
conn.execute_batch(
    "CREATE TABLE IF NOT EXISTS chat_store_version (version INTEGER NOT NULL);
     INSERT OR IGNORE INTO chat_store_version (rowid, version) VALUES (1, 0);"
)?;

let version: i64 = conn.query_row(
    "SELECT version FROM chat_store_version WHERE rowid = 1", [], |r| r.get(0)
)?;

if version < 1 {
    conn.execute_batch(
        "ALTER TABLE chat_messages RENAME TO _chat_messages_v0;
         CREATE TABLE chat_messages (
             id               INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id       TEXT    NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
             role             TEXT    NOT NULL CHECK(role IN ('user', 'assistant', 'tool')),
             content          TEXT    NOT NULL,
             tool_name        TEXT,
             tool_duration_ms INTEGER,
             created_at       TEXT    NOT NULL
         );
         INSERT INTO chat_messages (id, session_id, role, content, created_at)
             SELECT id, session_id, role, content, created_at FROM _chat_messages_v0;
         DROP TABLE _chat_messages_v0;
         CREATE INDEX IF NOT EXISTS idx_chat_msg_session ON chat_messages(session_id, id);
         UPDATE chat_store_version SET version = 1 WHERE rowid = 1;"
    )?;
}
```

3. Update the `ChatMessage` struct (line 19):

```rust
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,      // "user" | "assistant" | "tool"
    pub content: String,
    pub created_at: String,
    /// Tool name, populated when role == "tool".
    pub tool_name: Option<String>,
    /// Tool execution duration in milliseconds, populated when role == "tool".
    pub tool_duration_ms: Option<u64>,
}
```

4. Add the `add_tool_calls()` method. The `ChatToolCallRecord` type is imported from `agentos_kernel`:

```rust
/// Batch-insert tool call records for a session. Each tool call becomes a message
/// with `role = 'tool'`. Call this BEFORE saving the final assistant message so
/// the chronological ordering is: user -> tool1 -> tool2 -> ... -> assistant.
pub fn add_tool_calls(
    &self,
    session_id: &str,
    tool_calls: &[agentos_kernel::ChatToolCallRecord],
) -> Result<(), rusqlite::Error> {
    if tool_calls.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().to_rfc3339();
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    let tx = conn.unchecked_transaction()?;
    for tc in tool_calls {
        let content = serde_json::json!({
            "tool_name": tc.tool_name,
            "intent_type": tc.intent_type,
            "payload": tc.payload,
            "result": tc.result,
        })
        .to_string();
        tx.execute(
            "INSERT INTO chat_messages (session_id, role, content, tool_name, tool_duration_ms, created_at)
             VALUES (?1, 'tool', ?2, ?3, ?4, ?5)",
            rusqlite::params![session_id, content, tc.tool_name, tc.duration_ms as i64, now],
        )?;
    }
    tx.execute(
        "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now, session_id],
    )?;
    tx.commit()?;
    Ok(())
}
```

5. Update `get_messages()` (line 146) to select the new columns:

```rust
pub fn get_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>, rusqlite::Error> {
    let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
    let mut stmt = conn.prepare(
        "SELECT role, content, created_at, tool_name, tool_duration_ms
         FROM chat_messages
         WHERE session_id = ?1
         ORDER BY id DESC
         LIMIT 200",
    )?;
    let mut rows: Vec<ChatMessage> = stmt
        .query_map(rusqlite::params![session_id], |row| {
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

6. Open `crates/agentos-web/src/handlers/chat.rs`. In `new_session()` and `send()`, after calling `chat_infer_with_tools()`, save tool calls before the assistant message:

```rust
let result = match state.kernel.chat_infer_with_tools(&agent_name, &history_pairs, &message).await {
    Ok(r) => r,
    Err(e) => { /* existing error handling */ }
};

// Save tool call records (before assistant message for correct ordering).
if !result.tool_calls.is_empty() {
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    let calls = result.tool_calls.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || store.add_tool_calls(&sid, &calls)).await {
        tracing::error!("Failed to save tool calls: {e}");
    }
}

// Save assistant response.
let store = Arc::clone(&state.chat_store);
let sid = session_id.clone();
let answer = result.answer.clone();
if let Err(e) = tokio::task::spawn_blocking(move || store.add_message(&sid, "assistant", &answer)).await {
    tracing::error!("Failed to save assistant response: {e}");
}
```

7. In the `conversation()` handler, update the message context to include the new fields:

```rust
.map(|m| {
    context! {
        role => m.role,
        content => m.content,
        created_at => m.created_at,
        tool_name => m.tool_name.unwrap_or_default(),
        tool_duration_ms => m.tool_duration_ms.unwrap_or(0),
    }
})
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/chat_store.rs` | Schema versioning + migration v1; update `ChatMessage`; add `add_tool_calls()`; update `get_messages()` |
| `crates/agentos-web/src/handlers/chat.rs` | Save tool calls in `new_session()` and `send()`; update `conversation()` context |

---

## Prerequisites

[[28-01-Add Chat Tool Execution Loop to Kernel]] must be complete -- it provides `ChatToolCallRecord` and `chat_infer_with_tools()`.

---

## Test Plan

- `cargo test -p agentos-web` must pass.
- Add test `test_chat_store_tool_message_roundtrip`:
  - Create a temporary `ChatStore`.
  - Call `create_session_with_first_message("agent", "hello")`.
  - Create two `ChatToolCallRecord` values and call `add_tool_calls(session_id, &records)`.
  - Call `add_message(session_id, "assistant", "Here is the answer")`.
  - Call `get_messages(session_id)`.
  - Assert 4 messages: user, tool, tool, assistant -- in that order.
  - Assert `messages[1].role == "tool"` and `messages[1].tool_name == Some("agent-manual".into())`.
  - Assert `messages[1].tool_duration_ms` is populated.
- Add test `test_chat_store_migration_idempotent`:
  - Open `ChatStore` at a temp path. Add a message.
  - Drop the store.
  - Open `ChatStore` again at the same path.
  - Verify `get_messages()` still returns the message. Verify version is 1.
- Add test `test_chat_store_old_messages_have_none_tool_fields`:
  - Open a fresh store. Add a user and assistant message (no tool calls).
  - Call `get_messages()`. Verify `tool_name` is `None` and `tool_duration_ms` is `None` for both.

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web -- chat_store --nocapture
cargo clippy -p agentos-web -- -D warnings
```
