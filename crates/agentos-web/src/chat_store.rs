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
}

impl ChatStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys = ON;
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
            "INSERT INTO chat_sessions (id, agent_name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
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
        let mut stmt =
            conn.prepare("SELECT id, agent_name, updated_at FROM chat_sessions WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(ChatSession {
                id: row.get(0)?,
                agent_name: row.get(1)?,
                updated_at: row.get(2)?,
                last_preview: None,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn list_sessions(&self) -> Result<Vec<ChatSession>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT s.id, s.agent_name, s.updated_at,
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
                updated_at: row.get(2)?,
                last_preview: row.get(3)?,
            })
        })?;
        rows.collect()
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
                    tool_duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v.max(0) as u64),
                })
            })?
            .collect::<Result<_, _>>()?;
        // Reverse so the caller receives messages oldest-first.
        rows.reverse();
        Ok(rows)
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
                params![session_id, content, tc.tool_name, tc.duration_ms.min(i64::MAX as u64) as i64, now],
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
