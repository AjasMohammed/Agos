use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

/// SQLite-backed store for multi-agent conversations.
pub struct ConvoStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentConvo {
    pub id: String,
    pub topic: String,
    /// Ordered list of agent names (participants).
    pub participants: Vec<String>,
    pub max_turns: u32,
    /// "running" | "complete" | "stopped" | "error"
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConvoTurn {
    pub id: i64,
    pub turn_number: u32,
    pub agent_name: String,
    pub content: String,
    pub tool_call_count: u32,
    pub created_at: String,
}

impl ConvoStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS agent_convos (
                 id              TEXT PRIMARY KEY,
                 topic           TEXT NOT NULL,
                 participants    TEXT NOT NULL,
                 max_turns       INTEGER NOT NULL DEFAULT 10,
                 status          TEXT NOT NULL DEFAULT 'running',
                 created_at      TEXT NOT NULL,
                 updated_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_convos_updated
                 ON agent_convos(updated_at DESC);
             CREATE TABLE IF NOT EXISTS convo_turns (
                 id              INTEGER PRIMARY KEY AUTOINCREMENT,
                 convo_id        TEXT NOT NULL REFERENCES agent_convos(id) ON DELETE CASCADE,
                 turn_number     INTEGER NOT NULL,
                 agent_name      TEXT NOT NULL,
                 content         TEXT NOT NULL,
                 tool_call_count INTEGER NOT NULL DEFAULT 0,
                 created_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_convo_turns_convo
                 ON convo_turns(convo_id, turn_number);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn create_convo(
        &self,
        topic: &str,
        participants: &[String],
        max_turns: u32,
    ) -> Result<String, rusqlite::Error> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let participants_json = serde_json::to_string(participants)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO agent_convos (id, topic, participants, max_turns, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?5)",
            params![id, topic, participants_json, max_turns, now],
        )?;
        Ok(id)
    }

    pub fn get_convo(&self, id: &str) -> Result<Option<AgentConvo>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, topic, participants, max_turns, status, created_at, updated_at
             FROM agent_convos WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let participants_json: String = row.get(2)?;
            let participants: Vec<String> = match serde_json::from_str(&participants_json) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, "Corrupt participants JSON in convo row — returning empty list");
                    Vec::new()
                }
            };
            Ok(Some(AgentConvo {
                id: row.get(0)?,
                topic: row.get(1)?,
                participants,
                max_turns: row.get::<_, i64>(3)? as u32,
                status: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn list_convos(&self) -> Result<Vec<AgentConvo>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, topic, participants, max_turns, status, created_at, updated_at
             FROM agent_convos
             ORDER BY updated_at DESC
             LIMIT 100",
        )?;
        let rows = stmt.query_map([], |row| {
            let participants_json: String = row.get(2)?;
            let participants: Vec<String> = match serde_json::from_str(&participants_json) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, "Corrupt participants JSON in convo row — returning empty list");
                    Vec::new()
                }
            };
            Ok(AgentConvo {
                id: row.get(0)?,
                topic: row.get(1)?,
                participants,
                max_turns: row.get::<_, i64>(3)? as u32,
                status: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    pub fn add_turn(
        &self,
        convo_id: &str,
        turn_number: u32,
        agent_name: &str,
        content: &str,
        tool_call_count: u32,
    ) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO convo_turns (convo_id, turn_number, agent_name, content, tool_call_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![convo_id, turn_number, agent_name, content, tool_call_count, now],
        )?;
        tx.execute(
            "UPDATE agent_convos SET updated_at = ?1 WHERE id = ?2",
            params![now, convo_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_turns(&self, convo_id: &str) -> Result<Vec<ConvoTurn>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, turn_number, agent_name, content, tool_call_count, created_at
             FROM convo_turns
             WHERE convo_id = ?1
             ORDER BY turn_number ASC",
        )?;
        let rows = stmt.query_map(params![convo_id], |row| {
            Ok(ConvoTurn {
                id: row.get(0)?,
                turn_number: row.get::<_, i64>(1)? as u32,
                agent_name: row.get(2)?,
                content: row.get(3)?,
                tool_call_count: row.get::<_, i64>(4)? as u32,
                created_at: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    pub fn set_status(&self, convo_id: &str, status: &str) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE agent_convos SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, convo_id],
        )?;
        Ok(())
    }
}
