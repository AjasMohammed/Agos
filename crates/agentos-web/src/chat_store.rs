use agentos_kernel::kernel::ChatToolCallRecord;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

pub struct ChatStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct ChatSession {
    pub id: String,
    pub agent_name: String,
    /// Optional user-defined session title.
    pub title: Option<String>,
    pub updated_at: String,
    /// Last message preview (populated by `list_sessions`).
    pub last_preview: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant" | "tool"
    pub content: String,
    pub created_at: String,
    /// Tool name (populated when role == "tool").
    pub tool_name: Option<String>,
    /// Tool execution duration in milliseconds (populated when role == "tool").
    pub tool_duration_ms: Option<u64>,
    /// Tool call intent type (e.g. "query", "execute") when role == "tool".
    pub tool_intent_type: Option<String>,
    /// Tool input payload JSON string when role == "tool".
    pub tool_payload_json: Option<String>,
    /// Tool result payload JSON string when role == "tool".
    pub tool_result_json: Option<String>,
    /// Tool success flag when role == "tool".
    pub tool_success: Option<bool>,
}

/// A unified timeline entry for rendering the chat conversation.
/// Merges user/assistant messages with tool call records into a single
/// chronologically ordered stream.
#[derive(Debug, Clone)]
pub enum TimelineEntry {
    User {
        id: i64,
        content: String,
        created_at: String,
    },
    Assistant {
        id: i64,
        content: String,
        created_at: String,
        tokens_used: Option<u64>,
        cost_usd: Option<String>,
    },
    Tool {
        id: i64,
        tool_name: String,
        tool_intent_type: Option<String>,
        tool_payload_json: Option<String>,
        tool_result_json: Option<String>,
        tool_success: Option<bool>,
        tool_duration_ms: Option<u64>,
        created_at: String,
    },
}

impl ChatStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys = ON;
             -- v0 baseline schema — migrations below bring it to the current version.
             CREATE TABLE IF NOT EXISTS chat_sessions (
                 id          TEXT PRIMARY KEY,
                 agent_name  TEXT NOT NULL,
                 created_at  TEXT NOT NULL,
                 updated_at  TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS chat_messages (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id  TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
                 role        TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
                 content     TEXT NOT NULL,
                 created_at  TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_chat_msg_session
                 ON chat_messages(session_id, id);
             CREATE INDEX IF NOT EXISTS idx_chat_sess_updated
                 ON chat_sessions(updated_at DESC);
             -- Version table: id=1 is the single sentinel row.
             CREATE TABLE IF NOT EXISTS chat_store_version (id INTEGER PRIMARY KEY, version INTEGER NOT NULL DEFAULT 0);
             INSERT OR IGNORE INTO chat_store_version (id, version) VALUES (1, 0);",
        )?;

        // Migration v1: expand role constraint and add tool metadata columns.
        // Wrapped in BEGIN/COMMIT so a crash mid-migration leaves the DB unchanged.
        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 1 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE chat_messages RENAME TO chat_messages_old;
                 CREATE TABLE chat_messages (
                     id               INTEGER PRIMARY KEY AUTOINCREMENT,
                     session_id       TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
                     role             TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'tool')),
                     content          TEXT NOT NULL,
                     tool_name        TEXT,
                     tool_duration_ms INTEGER,
                     created_at       TEXT NOT NULL
                 );
                 INSERT INTO chat_messages (id, session_id, role, content, created_at)
                     SELECT id, session_id, role, content, created_at FROM chat_messages_old;
                 DROP TABLE chat_messages_old;
                 CREATE INDEX IF NOT EXISTS idx_chat_msg_session
                     ON chat_messages(session_id, id);
                 UPDATE chat_store_version SET version = 1 WHERE id = 1;
                 COMMIT;",
            )?;
        }

        // Migration v2: persist structured tool metadata columns and backfill existing
        // role='tool' rows that previously stored a JSON blob in `content`.
        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 2 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE chat_messages ADD COLUMN tool_intent_type TEXT;
                 ALTER TABLE chat_messages ADD COLUMN tool_payload_json TEXT;
                 ALTER TABLE chat_messages ADD COLUMN tool_result_json TEXT;
                 ALTER TABLE chat_messages ADD COLUMN tool_success INTEGER;
                 UPDATE chat_store_version SET version = 2 WHERE id = 1;
                 COMMIT;",
            )?;

            let mut stmt = conn.prepare(
                "SELECT id, content
                 FROM chat_messages
                 WHERE role = 'tool'
                   AND (tool_intent_type IS NULL OR tool_payload_json IS NULL OR tool_result_json IS NULL)",
            )?;
            let rows = stmt.query_map([], |row| {
                let id: i64 = row.get(0)?;
                let content: String = row.get(1)?;
                Ok((id, content))
            })?;
            let rows: Vec<(i64, String)> = rows.collect::<Result<_, _>>()?;
            drop(stmt);

            for (id, content) in rows {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
                    continue;
                };
                let intent_type = v
                    .get("intent_type")
                    .and_then(|x| x.as_str())
                    .map(str::to_string);
                let payload_json = v.get("payload").cloned().map(|x| x.to_string());
                let result_json = v.get("result").cloned().map(|x| x.to_string());
                let success = v.get("success").and_then(|x| x.as_bool()).or_else(|| {
                    v.get("result")
                        .and_then(|r| r.as_object())
                        .map(|obj| !obj.contains_key("error"))
                });

                let _ = conn.execute(
                    "UPDATE chat_messages
                     SET tool_intent_type = ?1,
                         tool_payload_json = ?2,
                         tool_result_json = ?3,
                         tool_success = ?4
                     WHERE id = ?5",
                    params![
                        intent_type,
                        payload_json,
                        result_json,
                        success.map(|s| if s { 1i64 } else { 0i64 }),
                        id
                    ],
                );
            }
        }

        // Migration v3: add optional session title for rename/fork UX.
        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 3 {
            // Column may already exist from a prior schema version — check first.
            let has_title: bool = conn
                .prepare("PRAGMA table_info(chat_sessions)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .any(|col| col.as_deref() == Ok("title"));
            if !has_title {
                conn.execute_batch("ALTER TABLE chat_sessions ADD COLUMN title TEXT;")?;
            }
            conn.execute_batch("UPDATE chat_store_version SET version = 3 WHERE id = 1;")?;
        }

        // Migration v4: introduce normalized chat_tool_calls table and backfill
        // from existing role='tool' chat_messages rows.
        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 4 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS chat_tool_calls (
                     id                INTEGER PRIMARY KEY AUTOINCREMENT,
                     message_id        INTEGER NOT NULL UNIQUE REFERENCES chat_messages(id) ON DELETE CASCADE,
                     tool_name         TEXT NOT NULL,
                     tool_intent_type  TEXT,
                     tool_payload_json TEXT,
                     tool_result_json  TEXT,
                     tool_duration_ms  INTEGER,
                     tool_success      INTEGER,
                     created_at        TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_chat_tool_calls_message
                     ON chat_tool_calls(message_id);
                 INSERT INTO chat_tool_calls (
                     message_id, tool_name, tool_intent_type, tool_payload_json,
                     tool_result_json, tool_duration_ms, tool_success, created_at
                 )
                 SELECT
                     m.id,
                     COALESCE(NULLIF(m.tool_name, ''), 'tool'),
                     m.tool_intent_type,
                     m.tool_payload_json,
                     m.tool_result_json,
                     m.tool_duration_ms,
                     m.tool_success,
                     m.created_at
                 FROM chat_messages m
                 WHERE m.role = 'tool'
                   AND NOT EXISTS (
                       SELECT 1 FROM chat_tool_calls c WHERE c.message_id = m.id
                   );
                 UPDATE chat_store_version SET version = 4 WHERE id = 1;
                 COMMIT;",
            )?;
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create a session and persist the first user message in a single transaction.
    pub fn create_session_with_first_message(
        &self,
        agent_name: &str,
        first_message: &str,
    ) -> Result<String, rusqlite::Error> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO chat_sessions (id, agent_name, title, created_at, updated_at)
             VALUES (?1, ?2, NULL, ?3, ?3)",
            params![id, agent_name, now],
        )?;
        tx.execute(
            "INSERT INTO chat_messages (session_id, role, content, created_at)
             VALUES (?1, 'user', ?2, ?3)",
            params![id, first_message, now],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<ChatSession>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn
            .prepare("SELECT id, agent_name, title, updated_at FROM chat_sessions WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(ChatSession {
                id: row.get(0)?,
                agent_name: row.get(1)?,
                title: row.get(2)?,
                updated_at: row.get(3)?,
                last_preview: None,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn list_sessions(&self) -> Result<Vec<ChatSession>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT s.id, s.agent_name, s.title, s.updated_at,
                    (SELECT content FROM chat_messages
                     WHERE session_id = s.id AND role IN ('user', 'assistant')
                     ORDER BY id DESC LIMIT 1) AS last_msg
             FROM chat_sessions s
             ORDER BY s.updated_at DESC
             LIMIT 100",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                agent_name: row.get(1)?,
                title: row.get(2)?,
                updated_at: row.get(3)?,
                last_preview: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    pub fn rename_session(&self, id: &str, title: Option<&str>) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let changed = conn.execute(
            "UPDATE chat_sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, id],
        )?;
        if changed == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let changed = conn.execute("DELETE FROM chat_sessions WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn fork_session(
        &self,
        source_session_id: &str,
        new_title: Option<&str>,
    ) -> Result<String, rusqlite::Error> {
        let new_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;

        let (agent_name, source_title): (String, Option<String>) = tx.query_row(
            "SELECT agent_name, title FROM chat_sessions WHERE id = ?1",
            params![source_session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let final_title = match new_title.map(str::trim).filter(|s| !s.is_empty()) {
            Some(v) => Some(v.to_string()),
            None => source_title.map(|t| format!("{t} (fork)")),
        };

        tx.execute(
            "INSERT INTO chat_sessions (id, agent_name, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![new_id, agent_name, final_title, now],
        )?;
        tx.execute(
            "INSERT INTO chat_messages (
                 session_id, role, content, tool_name, tool_duration_ms, tool_intent_type,
                 tool_payload_json, tool_result_json, tool_success, created_at
             )
             SELECT ?1, role, content, tool_name, tool_duration_ms, tool_intent_type,
                    tool_payload_json, tool_result_json, tool_success, created_at
             FROM chat_messages
             WHERE session_id = ?2
             ORDER BY id ASC",
            params![new_id, source_session_id],
        )?;
        tx.commit()?;
        Ok(new_id)
    }

    /// Add a message to an existing session. Both the INSERT and the session
    /// timestamp UPDATE are committed atomically in a single transaction.
    pub fn add_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<(), rusqlite::Error> {
        debug_assert!(
            role == "user" || role == "assistant" || role == "tool",
            "invalid chat role: {role}"
        );
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO chat_messages (session_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, role, content, now],
        )?;
        tx.execute(
            "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Return up to 200 most-recent messages for a session, in chronological order.
    pub fn get_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT m.role, m.content, m.created_at,
                    COALESCE(c.tool_name, m.tool_name) as tool_name,
                    COALESCE(c.tool_duration_ms, m.tool_duration_ms) as tool_duration_ms,
                    COALESCE(c.tool_intent_type, m.tool_intent_type) as tool_intent_type,
                    COALESCE(c.tool_payload_json, m.tool_payload_json) as tool_payload_json,
                    COALESCE(c.tool_result_json, m.tool_result_json) as tool_result_json,
                    COALESCE(c.tool_success, m.tool_success) as tool_success
             FROM chat_messages
             m
             LEFT JOIN chat_tool_calls c ON c.message_id = m.id
             WHERE m.session_id = ?1
             ORDER BY m.id DESC
             LIMIT 200",
        )?;
        let mut rows: Vec<ChatMessage> = stmt
            .query_map(params![session_id], |row| {
                Ok(ChatMessage {
                    role: row.get(0)?,
                    content: row.get(1)?,
                    created_at: row.get(2)?,
                    tool_name: row.get(3)?,
                    tool_duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v.max(0) as u64),
                    tool_intent_type: row.get(5)?,
                    tool_payload_json: row.get(6)?,
                    tool_result_json: row.get(7)?,
                    tool_success: row.get::<_, Option<i64>>(8)?.map(|v| v > 0),
                })
            })?
            .collect::<Result<_, _>>()?;
        // Reverse so the caller receives messages oldest-first.
        rows.reverse();
        Ok(rows)
    }

    /// Return the conversation as a chronologically ordered timeline of typed entries.
    /// User/assistant messages and tool calls are interleaved by their `created_at` /
    /// insertion order, giving the template a single list to iterate over.
    pub fn get_timeline(&self, session_id: &str) -> Result<Vec<TimelineEntry>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Fetch user + assistant messages.
        let mut msg_stmt = conn.prepare(
            "SELECT m.id, m.role, m.content, m.created_at
             FROM chat_messages m
             WHERE m.session_id = ?1 AND m.role IN ('user', 'assistant')
             ORDER BY m.id ASC
             LIMIT 500",
        )?;
        let msgs: Vec<(String, TimelineEntry)> = msg_stmt
            .query_map(params![session_id], |row| {
                let id: i64 = row.get(0)?;
                let role: String = row.get(1)?;
                let content: String = row.get(2)?;
                let created_at: String = row.get(3)?;
                let entry = match role.as_str() {
                    "user" => TimelineEntry::User {
                        id,
                        content,
                        created_at: created_at.clone(),
                    },
                    _ => TimelineEntry::Assistant {
                        id,
                        content,
                        created_at: created_at.clone(),
                        tokens_used: None,
                        cost_usd: None,
                    },
                };
                Ok((created_at, entry))
            })?
            .collect::<Result<_, _>>()?;

        // Fetch tool calls (from the normalized table, falling back to role='tool' messages).
        let mut tool_stmt = conn.prepare(
            "SELECT m.id,
                    COALESCE(c.tool_name, m.tool_name, 'unknown') as tool_name,
                    COALESCE(c.tool_intent_type, m.tool_intent_type) as tool_intent_type,
                    COALESCE(c.tool_payload_json, m.tool_payload_json) as tool_payload_json,
                    COALESCE(c.tool_result_json, m.tool_result_json) as tool_result_json,
                    COALESCE(c.tool_success, m.tool_success) as tool_success,
                    COALESCE(c.tool_duration_ms, m.tool_duration_ms) as tool_duration_ms,
                    m.created_at
             FROM chat_messages m
             LEFT JOIN chat_tool_calls c ON c.message_id = m.id
             WHERE m.session_id = ?1 AND m.role = 'tool'
             ORDER BY m.id ASC
             LIMIT 500",
        )?;
        let tools: Vec<(String, TimelineEntry)> = tool_stmt
            .query_map(params![session_id], |row| {
                let id: i64 = row.get(0)?;
                let tool_name: String = row.get(1)?;
                let tool_intent_type: Option<String> = row.get(2)?;
                let tool_payload_json: Option<String> = row.get(3)?;
                let tool_result_json: Option<String> = row.get(4)?;
                let tool_success: Option<bool> = row.get::<_, Option<i64>>(5)?.map(|v| v > 0);
                let tool_duration_ms: Option<u64> =
                    row.get::<_, Option<i64>>(6)?.map(|v| v.max(0) as u64);
                let created_at: String = row.get(7)?;
                Ok((
                    created_at.clone(),
                    TimelineEntry::Tool {
                        id,
                        tool_name,
                        tool_intent_type,
                        tool_payload_json,
                        tool_result_json,
                        tool_success,
                        tool_duration_ms,
                        created_at,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;

        // Merge by created_at, then by id for same-second entries.
        let mut all: Vec<(String, TimelineEntry)> = msgs;
        all.extend(tools);
        all.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(all.into_iter().map(|(_, e)| e).collect())
    }

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
            let payload_json = tc.payload.to_string();
            let result_json = tc.result.to_string();
            let success = !tc
                .result
                .as_object()
                .is_some_and(|obj| obj.contains_key("error"));
            let content = format!("Tool call: {}", tc.tool_name);
            tx.execute(
                "INSERT INTO chat_messages (
                     session_id, role, content, tool_name, tool_duration_ms,
                     tool_intent_type, tool_payload_json, tool_result_json, tool_success, created_at
                 )
                 VALUES (?1, 'tool', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    session_id,
                    content,
                    tc.tool_name,
                    tc.duration_ms.min(i64::MAX as u64) as i64,
                    tc.intent_type,
                    payload_json,
                    result_json,
                    if success { 1i64 } else { 0i64 },
                    now
                ],
            )?;
            let message_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO chat_tool_calls (
                     message_id, tool_name, tool_intent_type, tool_payload_json,
                     tool_result_json, tool_duration_ms, tool_success, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    message_id,
                    tc.tool_name,
                    tc.intent_type,
                    payload_json,
                    result_json,
                    tc.duration_ms.min(i64::MAX as u64) as i64,
                    if success { 1i64 } else { 0i64 },
                    now
                ],
            )?;
        }
        tx.execute(
            "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
