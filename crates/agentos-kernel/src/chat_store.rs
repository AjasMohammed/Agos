use crate::kernel::ChatToolCallRecord;
use rusqlite::{params, Connection, OptionalExtension};
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
    /// Number of messages in the session (populated by `list_sessions`; `0` from
    /// `get_session`, whose detail view carries the full message list instead).
    pub message_count: i64,
}

/// One full-text search hit from [`ChatStore::search`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatSearchHit {
    pub session_id: String,
    pub message_id: i64,
    /// "user" or "assistant".
    pub role: String,
    /// Message body, truncated to 400 chars.
    pub content: String,
    pub created_at: String,
    pub agent_name: String,
}

/// Truncate to at most `max` chars on a char boundary.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant" | "tool"
    pub content: String,
    /// Comma-separated upload UUIDs attached to this user message (LLM context), if any.
    pub file_ids: Option<String>,
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
        cost_usd: Option<f64>,
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
                        .filter(|r| r.is_object())
                        .map(|r| !crate::kernel::tool_result_is_error(r))
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

        // Migration v5: optional file attachment IDs per user message (multi-turn LLM context).
        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 5 {
            let has_file_ids: bool = conn
                .prepare("PRAGMA table_info(chat_messages)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .any(|col| col.as_deref() == Ok("file_ids"));
            if !has_file_ids {
                conn.execute_batch("ALTER TABLE chat_messages ADD COLUMN file_ids TEXT;")?;
            }
            conn.execute_batch("UPDATE chat_store_version SET version = 5 WHERE id = 1;")?;
        }

        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 6 {
            let columns: Vec<String> = conn
                .prepare("PRAGMA table_info(chat_messages)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<_, _>>()?;
            if !columns.iter().any(|col| col == "tokens_used") {
                conn.execute_batch("ALTER TABLE chat_messages ADD COLUMN tokens_used INTEGER;")?;
            }
            if !columns.iter().any(|col| col == "cost_usd") {
                conn.execute_batch("ALTER TABLE chat_messages ADD COLUMN cost_usd REAL;")?;
            }
            conn.execute_batch("UPDATE chat_store_version SET version = 6 WHERE id = 1;")?;
        }

        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 8 {
            let columns: Vec<String> = conn
                .prepare("PRAGMA table_info(chat_messages)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<_, _>>()?;
            if !columns.iter().any(|col| col == "cost_usd") {
                conn.execute_batch("ALTER TABLE chat_messages ADD COLUMN cost_usd REAL;")?;
            }
            conn.execute_batch("UPDATE chat_store_version SET version = 8 WHERE id = 1;")?;
        }

        // Migration v9: full-text index over message bodies so agents can
        // search their own past sessions (`chat-search` tool). External-content
        // FTS5 table — the triggers keep it in sync, and `rebuild` backfills
        // everything already stored.
        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 9 {
            conn.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS chat_messages_fts
                     USING fts5(content, content='chat_messages', content_rowid='id');

                 CREATE TRIGGER IF NOT EXISTS chat_messages_ai AFTER INSERT ON chat_messages BEGIN
                     INSERT INTO chat_messages_fts(rowid, content) VALUES (new.id, new.content);
                 END;
                 CREATE TRIGGER IF NOT EXISTS chat_messages_ad AFTER DELETE ON chat_messages BEGIN
                     INSERT INTO chat_messages_fts(chat_messages_fts, rowid, content)
                     VALUES ('delete', old.id, old.content);
                 END;
                 CREATE TRIGGER IF NOT EXISTS chat_messages_au AFTER UPDATE ON chat_messages BEGIN
                     INSERT INTO chat_messages_fts(chat_messages_fts, rowid, content)
                     VALUES ('delete', old.id, old.content);
                     INSERT INTO chat_messages_fts(rowid, content) VALUES (new.id, new.content);
                 END;

                 INSERT INTO chat_messages_fts(chat_messages_fts) VALUES ('rebuild');
                 UPDATE chat_store_version SET version = 9 WHERE id = 1;",
            )?;
        }

        // Migration v10: bind a session to an external channel conversation.
        // Channel chat (Telegram/Discord/…) used to keep its transcript in a
        // process-local map, so every kernel restart wiped the conversation and
        // none of it was searchable. Channel turns now land in a normal session
        // keyed by this column, which makes them visible to the panel, to
        // `chat-search`, and to session-scoped tool state.
        let version: i64 = conn.query_row(
            "SELECT version FROM chat_store_version WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        if version < 10 {
            let has_channel_key: bool = conn
                .prepare("PRAGMA table_info(chat_sessions)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .any(|col| col.as_deref() == Ok("channel_key"));
            if !has_channel_key {
                conn.execute_batch("ALTER TABLE chat_sessions ADD COLUMN channel_key TEXT;")?;
            }
            // Partial unique index: at most one *live* session per channel key,
            // while rotated sessions (channel_key NULL) accumulate freely.
            conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_chat_sess_channel_key
                     ON chat_sessions(channel_key) WHERE channel_key IS NOT NULL;
                 UPDATE chat_store_version SET version = 10 WHERE id = 1;",
            )?;
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Full-text search across stored chat messages, newest first.
    ///
    /// Only `user` and `assistant` turns are searchable — tool rows are
    /// machine payloads and would drown the results. `agent_name` scopes the
    /// search to one agent's sessions (an agent should not read another's
    /// conversations); `None` searches everything and is used only by
    /// operator-facing callers.
    pub fn search(
        &self,
        query: &str,
        agent_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChatSearchHit>, rusqlite::Error> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        // Quote the whole query as one FTS5 phrase so user punctuation can't
        // be parsed as MATCH syntax (and can't error the statement).
        let phrase = format!("\"{}\"", trimmed.replace('"', "\"\""));
        let limit = limit.clamp(1, 100) as i64;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT m.session_id, m.id, m.role, m.content, m.created_at, s.agent_name
               FROM chat_messages_fts f
               JOIN chat_messages m ON m.id = f.rowid
               JOIN chat_sessions s ON s.id = m.session_id
              WHERE chat_messages_fts MATCH ?1
                AND m.role IN ('user', 'assistant')
                AND (?2 IS NULL OR s.agent_name = ?2)
              ORDER BY m.id DESC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![phrase, agent_name, limit], |row| {
            let content: String = row.get(3)?;
            Ok(ChatSearchHit {
                session_id: row.get(0)?,
                message_id: row.get(1)?,
                role: row.get(2)?,
                content: truncate_chars(&content, 400),
                created_at: row.get(4)?,
                agent_name: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// Create an empty session — no messages.
    ///
    /// The panel opens a chat lazily: the session row is written when the user
    /// actually sends, and the send path persists the user turn itself. Creating
    /// a session with a blank placeholder message instead (what the API used to
    /// do for a missing `first_message`) put an empty user row at the head of the
    /// transcript, which then got replayed to the LLM as history.
    pub fn create_session(&self, agent_name: &str) -> Result<String, rusqlite::Error> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO chat_sessions (id, agent_name, title, created_at, updated_at)
             VALUES (?1, ?2, NULL, ?3, ?3)",
            params![id, agent_name, now],
        )?;
        Ok(id)
    }

    /// Create a session and persist the first user message in a single transaction.
    pub fn create_session_with_first_message(
        &self,
        agent_name: &str,
        first_message: &str,
        file_ids: Option<&str>,
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
            "INSERT INTO chat_messages (session_id, role, content, file_ids, created_at)
             VALUES (?1, 'user', ?2, ?3, ?4)",
            params![id, first_message, file_ids, now],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Resolve the persistent session backing an external channel conversation,
    /// creating it on first use.
    ///
    /// `channel_key` is the stable identity of the conversation (channel
    /// instance + bound agent). Because it is a real session, the channel
    /// transcript survives kernel restarts and shows up everywhere web chat
    /// does — the panel, `chat-search`, and session-scoped tool state.
    pub fn get_or_create_channel_session(
        &self,
        channel_key: &str,
        agent_name: &str,
        title: &str,
    ) -> Result<String, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(id) = conn
            .query_row(
                "SELECT id FROM chat_sessions WHERE channel_key = ?1",
                params![channel_key],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO chat_sessions (id, agent_name, title, channel_key, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, agent_name, title, channel_key, now],
        )?;
        Ok(id)
    }

    /// Detach `channel_key` from its session so the next channel message starts
    /// a fresh one.
    ///
    /// Deliberately *not* a delete: rebinding a channel to another agent should
    /// end the thread, not destroy what the user and the agent already said.
    /// The orphaned session stays browsable and searchable.
    pub fn rotate_channel_session(&self, channel_key: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE chat_sessions SET channel_key = NULL WHERE channel_key = ?1",
            params![channel_key],
        )?;
        Ok(())
    }

    /// Channel sessions with no assistant reply after their last inbound turn,
    /// newer than `since_rfc3339` — i.e. a channel turn the kernel was killed in
    /// the middle of. Returns `(session_id, channel_key)`.
    ///
    /// Chat turns are not checkpointed (unlike `AgentTask`), so a restart mid-turn
    /// drops the reply silently and the user is left waiting forever. The boot
    /// sweep uses this to tell them to resend.
    ///
    /// A trailing `tool` row counts too: tool calls are persisted just before the
    /// assistant turn (`channel_chat_bridge::channel_chat`), so a kill landing
    /// between the two writes leaves `user → tool` and still means "no answer".
    /// A turn that merely *failed* is excluded, because the failure is itself
    /// persisted as an assistant row.
    ///
    /// `since_rfc3339` must use the same UTC offset spelling as the stored
    /// timestamps (`chrono::Utc::now().to_rfc3339()`, i.e. `+00:00`) — the
    /// comparison is lexicographic.
    pub fn channel_sessions_awaiting_reply(
        &self,
        since_rfc3339: &str,
    ) -> Result<Vec<(String, String)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT s.id, s.channel_key
             FROM chat_sessions s
             JOIN chat_messages m
               ON m.id = (SELECT MAX(id) FROM chat_messages WHERE session_id = s.id)
             WHERE s.channel_key IS NOT NULL
               AND m.role IN ('user', 'tool')
               AND m.created_at >= ?1",
        )?;
        let rows = stmt.query_map(params![since_rfc3339], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect()
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
                message_count: 0,
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
                     ORDER BY id DESC LIMIT 1) AS last_msg,
                    (SELECT COUNT(*) FROM chat_messages
                     WHERE session_id = s.id) AS msg_count
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
                message_count: row.get(5)?,
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
                 session_id, role, content, file_ids, tool_name, tool_duration_ms, tool_intent_type,
                 tool_payload_json, tool_result_json, tool_success, created_at
             )
             SELECT ?1, role, content, file_ids, tool_name, tool_duration_ms, tool_intent_type,
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
    ///
    /// `file_ids` is only stored for `role == "user"` (comma-separated upload UUIDs).
    pub fn add_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        file_ids: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        debug_assert!(
            role == "user" || role == "assistant" || role == "tool",
            "invalid chat role: {role}"
        );
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        let fid = if role == "user" { file_ids } else { None };
        tx.execute(
            "INSERT INTO chat_messages (session_id, role, content, file_ids, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, role, content, fid, now],
        )?;
        tx.execute(
            "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_assistant_message(
        &self,
        session_id: &str,
        content: &str,
        tokens_used: Option<u64>,
        cost_usd: Option<f64>,
    ) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO chat_messages (session_id, role, content, tokens_used, cost_usd, created_at)
             VALUES (?1, 'assistant', ?2, ?3, ?4, ?5)",
            params![
                session_id,
                content,
                tokens_used.map(|v| v.min(i64::MAX as u64) as i64),
                cost_usd,
                now
            ],
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
            "SELECT m.role, m.content, m.file_ids, m.created_at,
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
                    file_ids: row.get(2)?,
                    created_at: row.get(3)?,
                    tool_name: row.get(4)?,
                    tool_duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                    tool_intent_type: row.get(6)?,
                    tool_payload_json: row.get(7)?,
                    tool_result_json: row.get(8)?,
                    tool_success: row.get::<_, Option<i64>>(9)?.map(|v| v > 0),
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
            "SELECT m.id, m.role, m.content, m.created_at, m.tokens_used, m.cost_usd
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
                let tokens_used: Option<u64> =
                    row.get::<_, Option<i64>>(4)?.map(|v| v.max(0) as u64);
                let cost_usd: Option<f64> = row.get(5)?;
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
                        tokens_used,
                        cost_usd,
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

    /// Distinct tool names called in `session_id`, most recent first — every
    /// persisted call, including failures and tools the dedup cache skips
    /// (volatile, approval-gated). Feeds the chat working-set pins.
    pub fn recent_tool_names(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<String>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT COALESCE(c.tool_name, m.tool_name) AS name
             FROM chat_messages m
             LEFT JOIN chat_tool_calls c ON c.message_id = m.id
             WHERE m.session_id = ?1 AND m.role = 'tool'
               AND COALESCE(c.tool_name, m.tool_name) IS NOT NULL
             GROUP BY name
             ORDER BY MAX(m.id) DESC
             LIMIT ?2",
        )?;
        let names = stmt
            .query_map(params![session_id, limit as i64], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(names)
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
            let success = !crate::kernel::tool_result_is_error(&tc.result);
            let content = format!("Tool call: {}", tc.tool_name);
            // RETURNING, not `last_insert_rowid()`: the `chat_messages_ai` FTS
            // trigger inserts into `chat_messages_fts` after this row, so the
            // rowid it would report is the FTS row's — the FK below then fails.
            let message_id: i64 = tx.query_row(
                "INSERT INTO chat_messages (
                     session_id, role, content, tool_name, tool_duration_ms,
                     tool_intent_type, tool_payload_json, tool_result_json, tool_success, created_at
                 )
                 VALUES (?1, 'tool', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 RETURNING id",
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
                |row| row.get(0),
            )?;
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

#[cfg(test)]
mod tests {

    #[test]
    fn search_finds_messages_scoped_to_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(&dir.path().join("chat.db")).expect("open");
        let a = store
            .create_session_with_first_message("alpha", "where is the staging database?", None)
            .unwrap();
        store
            .add_assistant_message(&a, "The staging database lives at 10.0.0.42", None, None)
            .unwrap();
        let b = store
            .create_session_with_first_message("beta", "staging database question", None)
            .unwrap();
        assert!(!b.is_empty());

        let hits = store.search("staging database", Some("alpha"), 10).unwrap();
        assert_eq!(hits.len(), 2, "both alpha turns match");
        assert!(hits.iter().all(|h| h.session_id == a));
        assert!(hits.iter().all(|h| h.agent_name == "alpha"));

        // Newest first.
        assert_eq!(hits[0].role, "assistant");

        // Cross-agent isolation.
        let beta = store.search("staging database", Some("beta"), 10).unwrap();
        assert_eq!(beta.len(), 1);

        // Punctuation is treated as a literal phrase, never as MATCH syntax.
        assert!(store.search("what? (staging)", Some("alpha"), 5).is_ok());
        assert!(store.search("   ", Some("alpha"), 5).unwrap().is_empty());
    }
    use super::*;

    /// Working-set pins read every persisted call, newest first, distinct —
    /// including failures, which the in-memory dedup cache never holds.
    #[test]
    fn recent_tool_names_distinct_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ChatStore::open(&dir.path().join("chat.db")).expect("open");
        let sid = store.create_session("alpha").expect("create");
        let call = |name: &str, result: serde_json::Value| crate::kernel::ChatToolCallRecord {
            tool_name: name.to_string(),
            intent_type: String::new(),
            id: None,
            payload: serde_json::json!({}),
            result,
            duration_ms: 1,
        };
        store
            .add_tool_calls(
                &sid,
                &[
                    call("shell-exec", serde_json::json!({"error": "denied"})),
                    call("web-fetch", serde_json::json!({"ok": true})),
                    call("shell-exec", serde_json::json!({"ok": true})),
                ],
            )
            .expect("add");
        let names = store.recent_tool_names(&sid, 10).expect("names");
        assert_eq!(names, vec!["shell-exec", "web-fetch"]);
        assert_eq!(store.recent_tool_names(&sid, 1).expect("names").len(), 1);
        assert!(store
            .recent_tool_names("other", 10)
            .expect("names")
            .is_empty());
    }

    /// A lazily-opened chat: session row, zero messages, and the first send is
    /// the first row — no blank placeholder turn at the head of the transcript.
    #[test]
    fn create_session_starts_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ChatStore::open(&dir.path().join("chat.db")).expect("open");
        let sid = store.create_session("alpha").expect("create");

        assert!(store.get_messages(&sid).expect("get").is_empty());
        let session = store
            .get_session(&sid)
            .expect("get session")
            .expect("exists");
        assert_eq!(session.agent_name, "alpha");

        store
            .add_message(&sid, "user", "hello", None)
            .expect("send");
        let msgs = store.get_messages(&sid).expect("get");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn persists_and_reads_file_ids_on_user_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("chat.db");
        let store = ChatStore::open(&db).expect("open");
        let fid = "550e8400-e29b-41d4-a716-446655440000";
        let sid = store
            .create_session_with_first_message("agent", "summarize this", Some(fid))
            .expect("create");

        let msgs = store.get_messages(&sid).expect("get");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "summarize this");
        assert_eq!(msgs[0].file_ids.as_deref(), Some(fid));

        store
            .add_message(&sid, "assistant", "done", None)
            .expect("assistant");
        store
            .add_message(&sid, "user", "translate", Some(fid))
            .expect("user2");

        let msgs = store.get_messages(&sid).expect("get2");
        let users: Vec<_> = msgs.iter().filter(|m| m.role == "user").collect();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].content, "summarize this");
        assert_eq!(users[0].file_ids.as_deref(), Some(fid));
        assert_eq!(users[1].content, "translate");
        assert_eq!(users[1].file_ids.as_deref(), Some(fid));
    }

    #[test]
    fn fork_session_copies_file_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("chat.db");
        let store = ChatStore::open(&db).expect("open");
        let fid = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
        let sid = store
            .create_session_with_first_message("a", "m", Some(fid))
            .expect("create");
        let forked = store.fork_session(&sid, None).expect("fork");
        let msgs = store.get_messages(&forked).expect("get fork");
        assert_eq!(msgs[0].file_ids.as_deref(), Some(fid));
    }

    #[test]
    fn awaiting_reply_finds_only_unanswered_recent_channel_turns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ChatStore::open(&dir.path().join("chat.db")).expect("open");
        let cutoff = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();

        // Killed mid-turn: trailing user row.
        let stuck = store
            .get_or_create_channel_session("channel:c1:ops", "ops", "t")
            .expect("create");
        store
            .add_message(&stuck, "user", "any audio on Desktop?", None)
            .expect("add");

        // Answered: trailing assistant row.
        let done = store
            .get_or_create_channel_session("channel:c2:ops", "ops", "t")
            .expect("create");
        store.add_message(&done, "user", "hi", None).expect("add");
        store
            .add_assistant_message(&done, "hello", None, None)
            .expect("add");

        // Killed between persisting tool calls and the assistant turn.
        let mid_tool = store
            .get_or_create_channel_session("channel:c4:ops", "ops", "t")
            .expect("create");
        store
            .add_message(&mid_tool, "user", "list files", None)
            .expect("add");
        store
            .add_message(&mid_tool, "tool", "Tool call: file-list", None)
            .expect("add");

        // Web chat (no channel_key) is out of scope — it has no channel to reply to.
        let web = store.create_session("ops").expect("create");
        store
            .add_message(&web, "user", "orphan", None)
            .expect("add");

        let mut pending = store
            .channel_sessions_awaiting_reply(&cutoff)
            .expect("scan");
        pending.sort();
        let mut expected = vec![
            (stuck.clone(), "channel:c1:ops".to_string()),
            (mid_tool.clone(), "channel:c4:ops".to_string()),
        ];
        expected.sort();
        assert_eq!(pending, expected);

        // Persisting the notice is what stops the next boot re-announcing it.
        for sid in [&stuck, &mid_tool] {
            store
                .add_assistant_message(sid, "I was restarted", None, None)
                .expect("add");
        }
        assert!(store
            .channel_sessions_awaiting_reply(&cutoff)
            .expect("scan")
            .is_empty());

        // Old stuck turns stay quiet.
        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let fresh = store
            .get_or_create_channel_session("channel:c3:ops", "ops", "t")
            .expect("create");
        store
            .add_message(&fresh, "user", "recent", None)
            .expect("add");
        assert!(store
            .channel_sessions_awaiting_reply(&future)
            .expect("scan")
            .is_empty());
    }
}
