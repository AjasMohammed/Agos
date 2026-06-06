//! SQLite-backed store for `claude` CLI session ids, keyed by `ContextID`.
//!
//! Backs the opt-in `--resume` mode of the claude-code adapter (see
//! `agentos_llm::session`). The stored session is treated strictly as a
//! **cache**: it is invalidated on context compaction and deleted on task
//! completion, so the kernel never cedes authority over conversation state.
//!
//! All I/O runs on `spawn_blocking` (rusqlite is synchronous). Mirrors the
//! conventions in `task_state_store.rs` (WAL, `busy_timeout`).

use agentos_types::AgentOSError;
use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS claude_sessions (
    context_id            TEXT PRIMARY KEY,
    session_id            TEXT NOT NULL,
    last_sent_entry_count INTEGER NOT NULL DEFAULT 0,
    updated_at            TEXT NOT NULL
);";

/// SQLite store mapping a `ContextID` to its claude CLI session id + the number
/// of context entries already sent (high-water mark for delta resume).
pub struct ClaudeSessionStore {
    conn: Arc<Mutex<Connection>>,
}

impl ClaudeSessionStore {
    /// Open (or create) the session database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open claude_session.db: {e}"))
        })?;
        Self::init(conn)
    }

    /// In-memory store — fallback when the on-disk DB can't be opened, and for tests.
    pub fn in_memory() -> Result<Self, AgentOSError> {
        let conn = Connection::open_in_memory().map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open in-memory claude_session: {e}"))
        })?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, AgentOSError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(|e| AgentOSError::StorageError(format!("PRAGMA setup failed: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| AgentOSError::StorageError(format!("Schema creation failed: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Fetch `(session_id, last_sent_entry_count)` for a context, if present.
    pub async fn get(&self, context_id: &str) -> Result<Option<(String, usize)>, AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let ctx = context_id.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("lock poisoned: {e}")))?;
            let row = guard
                .query_row(
                    "SELECT session_id, last_sent_entry_count FROM claude_sessions WHERE context_id = ?1",
                    params![ctx],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(|e| AgentOSError::StorageError(format!("query claude_session: {e}")))?;
            Ok(row.map(|(sid, n)| (sid, n.max(0) as usize)))
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("spawn_blocking join: {e}")))?
    }

    /// UPSERT the session id + high-water mark for a context.
    pub async fn put(
        &self,
        context_id: &str,
        session_id: &str,
        last_sent_entry_count: usize,
    ) -> Result<(), AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let ctx = context_id.to_string();
        let sid = session_id.to_string();
        let n = last_sent_entry_count as i64;
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("lock poisoned: {e}")))?;
            guard
                .execute(
                    "INSERT INTO claude_sessions
                       (context_id, session_id, last_sent_entry_count, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(context_id) DO UPDATE SET
                       session_id = excluded.session_id,
                       last_sent_entry_count = excluded.last_sent_entry_count,
                       updated_at = excluded.updated_at",
                    params![ctx, sid, n, now],
                )
                .map_err(|e| AgentOSError::StorageError(format!("upsert claude_session: {e}")))?;
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("spawn_blocking join: {e}")))?
    }

    /// Delete the stored session for a context. Returns whether a row was removed.
    pub async fn delete(&self, context_id: &str) -> Result<bool, AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let ctx = context_id.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("lock poisoned: {e}")))?;
            let n = guard
                .execute(
                    "DELETE FROM claude_sessions WHERE context_id = ?1",
                    params![ctx],
                )
                .map_err(|e| AgentOSError::StorageError(format!("delete claude_session: {e}")))?;
            Ok(n > 0)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("spawn_blocking join: {e}")))?
    }
}

/// Kernel-side implementation of the adapter's `ClaudeSessionLookup` trait,
/// wrapping [`ClaudeSessionStore`]. All methods are best-effort: store errors are
/// logged and swallowed (the session is a cache, never a source of truth), so a
/// failing DB degrades gracefully to stateless full-context sends.
pub struct KernelClaudeSessionLookup {
    store: Arc<ClaudeSessionStore>,
}

impl KernelClaudeSessionLookup {
    pub fn new(store: Arc<ClaudeSessionStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl agentos_llm::ClaudeSessionLookup for KernelClaudeSessionLookup {
    async fn lookup(&self, context_id: &str) -> Option<agentos_llm::SessionState> {
        match self.store.get(context_id).await {
            Ok(Some((session_id, last_sent_entry_count))) => Some(agentos_llm::SessionState {
                session_id,
                last_sent_entry_count,
            }),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, context_id, "claude session lookup failed");
                None
            }
        }
    }

    async fn record(&self, context_id: &str, session_id: &str, sent_entry_count: usize) {
        if let Err(e) = self
            .store
            .put(context_id, session_id, sent_entry_count)
            .await
        {
            tracing::warn!(error = %e, context_id, "claude session record failed");
        }
    }

    async fn invalidate(&self, context_id: &str) {
        if let Err(e) = self.store.delete(context_id).await {
            tracing::warn!(error = %e, context_id, "claude session invalidate failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_get_put_delete() {
        let s = ClaudeSessionStore::in_memory().unwrap();
        assert!(s.get("ctx1").await.unwrap().is_none());

        s.put("ctx1", "sess-A", 5).await.unwrap();
        assert_eq!(
            s.get("ctx1").await.unwrap(),
            Some(("sess-A".to_string(), 5))
        );

        // UPSERT replaces session id + high-water mark.
        s.put("ctx1", "sess-B", 9).await.unwrap();
        assert_eq!(
            s.get("ctx1").await.unwrap(),
            Some(("sess-B".to_string(), 9))
        );

        assert!(s.delete("ctx1").await.unwrap());
        assert!(!s.delete("ctx1").await.unwrap());
        assert!(s.get("ctx1").await.unwrap().is_none());
    }
}
