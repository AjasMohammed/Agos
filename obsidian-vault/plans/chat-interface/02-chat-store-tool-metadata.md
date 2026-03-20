---
title: Chat Store Tool Metadata
tags:
  - web
  - v3
  - plan
date: 2026-03-18
status: planned
effort: 0.5d
priority: high
---

# Phase 02 -- Chat Store Tool Metadata

> Extend the ChatStore SQLite schema to persist tool call activity alongside chat messages, so tool calls are visible when the user revisits a conversation.

---

## Why This Phase

After Phase 01, the kernel returns `ChatInferenceResult` with a `tool_calls: Vec<ChatToolCallRecord>` field. But the ChatStore schema (`chat_store.rs`) only supports `role IN ('user', 'assistant')`. Tool call records need to be persisted so:

1. The conversation template can render tool activity indicators (Phase 04).
2. History sent back to the LLM on subsequent messages includes tool results.
3. Users can see what tools were called when they revisit a session.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `chat_messages.role` CHECK | `'user', 'assistant'` | `'user', 'assistant', 'tool'` |
| Tool call data | Not stored | Stored as `role='tool'`, `content` = JSON with tool_name, result, duration |
| `ChatMessage` struct | `role: String, content: String, created_at: String` | Add `tool_name: Option<String>`, `tool_duration_ms: Option<u64>` |
| `ChatStore::add_tool_calls()` | Does not exist | New method to batch-insert tool records |
| `ChatStore::get_messages()` | Returns all messages | Returns all messages including `role='tool'` entries |

---

## What to Do

### Step 1: Migrate the schema

Open `crates/agentos-web/src/chat_store.rs`. In the `open()` method, add a migration after the existing `CREATE TABLE IF NOT EXISTS` block:

```rust
// Migration: expand role constraint to include 'tool'.
// SQLite cannot ALTER CHECK constraints, so we use a pragmatic approach:
// drop and recreate the constraint only if needed. Since this is WAL mode
// and the table might already exist, we use a conditional migration.
conn.execute_batch(
    "CREATE TABLE IF NOT EXISTS chat_store_version (version INTEGER);
     INSERT OR IGNORE INTO chat_store_version VALUES (0);"
)?;

let version: i64 = conn.query_row(
    "SELECT version FROM chat_store_version LIMIT 1", [], |r| r.get(0)
)?;

if version < 1 {
    // Recreate chat_messages with expanded role constraint
    conn.execute_batch(
        "ALTER TABLE chat_messages RENAME TO chat_messages_old;
         CREATE TABLE chat_messages (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id  TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
             role        TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'tool')),
             content     TEXT NOT NULL,
             tool_name   TEXT,
             tool_duration_ms INTEGER,
             created_at  TEXT NOT NULL
         );
         INSERT INTO chat_messages (id, session_id, role, content, created_at)
             SELECT id, session_id, role, content, created_at FROM chat_messages_old;
         DROP TABLE chat_messages_old;
         CREATE INDEX IF NOT EXISTS idx_chat_msg_session
             ON chat_messages(session_id, id);
         UPDATE chat_store_version SET version = 1;"
    )?;
}
```

### Step 2: Update `ChatMessage` struct

```rust
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant" | "tool"
    pub content: String,
    pub created_at: String,
    /// Tool name (populated when role == "tool").
    pub tool_name: Option<String>,
    /// Tool execution duration in milliseconds (populated when role == "tool").
    pub tool_duration_ms: Option<u64>,
}
```

### Step 3: Add `add_tool_calls()` method

```rust
/// Batch-insert tool call records for a session. Each tool call becomes a
/// message with role='tool'. Call this before saving the final assistant message
/// so the message ordering is: user -> tool1 -> tool2 -> ... -> assistant.
pub fn add_tool_calls(
    &self,
    session_id: &str,
    tool_calls: &[ChatToolCallRecord],
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
        }).to_string();
        tx.execute(
            "INSERT INTO chat_messages (session_id, role, content, tool_name, tool_duration_ms, created_at)
             VALUES (?1, 'tool', ?2, ?3, ?4, ?5)",
            params![session_id, content, tc.tool_name, tc.duration_ms as i64, now],
        )?;
    }
    tx.execute(
        "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
        params![now, session_id],
    )?;
    tx.commit()?;
    Ok(())
}
```

Note: `ChatToolCallRecord` is defined in `agentos-kernel`. The web crate already depends on `agentos-kernel`, so this type is accessible. Import it in `chat_store.rs`:

```rust
use agentos_kernel::kernel::{ChatToolCallRecord};
```

Or, if that module path is not public, the handler can convert the records to a simpler form before passing to `add_tool_calls`.

### Step 4: Update `get_messages()` to include new columns

```rust
let mut stmt = conn.prepare(
    "SELECT role, content, created_at, tool_name, tool_duration_ms
     FROM chat_messages
     WHERE session_id = ?1
     ORDER BY id DESC
     LIMIT 200",
)?;
let mut rows: Vec<ChatMessage> = stmt
    .query_map(params![session_id], |row| {
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
```

### Step 5: Update chat handler to save tool calls

In `crates/agentos-web/src/handlers/chat.rs`, after calling `chat_infer_with_tools()`, save tool calls before saving the assistant message:

```rust
// Save tool call records.
if !result.tool_calls.is_empty() {
    let store = Arc::clone(&state.chat_store);
    let sid = session_id.clone();
    let calls = result.tool_calls.clone();
    let _ = tokio::task::spawn_blocking(move || store.add_tool_calls(&sid, &calls)).await;
}

// Save assistant response.
let store = Arc::clone(&state.chat_store);
let sid = session_id.clone();
let answer = result.answer.clone();
let _ = tokio::task::spawn_blocking(move || store.add_message(&sid, "assistant", &answer)).await;
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/chat_store.rs` | Schema migration; update `ChatMessage`; add `add_tool_calls()`; update `get_messages()` |
| `crates/agentos-web/src/handlers/chat.rs` | Save tool calls before assistant message |

---

## Dependencies

[[01-chat-tool-execution-loop]] must be complete (provides `ChatToolCallRecord` type and `chat_infer_with_tools()`).

---

## Test Plan

- `cargo test -p agentos-web` must pass.
- Add test `test_chat_store_tool_message_roundtrip`: Create a session, add a tool call via `add_tool_calls()`, add an assistant message, call `get_messages()`, verify ordering and that `tool_name` / `tool_duration_ms` are populated.
- Add test `test_chat_store_migration_idempotent`: Open the same database twice; verify the migration runs only once (version check).
- Verify existing sessions still load correctly after migration (all `tool_name` / `tool_duration_ms` are `None` for old rows).

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web -- chat_store --nocapture
cargo clippy -p agentos-web -- -D warnings
```
