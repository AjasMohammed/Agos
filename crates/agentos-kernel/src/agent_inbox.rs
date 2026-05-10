use agentos_storage::{Migrations, StoreHandle};
use agentos_types::{AgentID, AgentInboxEntry, AgentInboxEntryID, AgentInboxKind, AgentOSError};
use chrono::Utc;
use rusqlite::params;
use std::path::Path;

/// Maximum entries evicted in a single write-time cap sweep.
const PURGE_BATCH: usize = 32;

const MIGRATIONS: Migrations = &["CREATE TABLE IF NOT EXISTS agent_inbox (
        id          TEXT PRIMARY KEY,
        agent_id    TEXT NOT NULL,
        kind        TEXT NOT NULL,
        title       TEXT NOT NULL,
        body        TEXT NOT NULL,
        ref_id      TEXT,
        created_at  TEXT NOT NULL,
        expires_at  TEXT,
        read        INTEGER NOT NULL DEFAULT 0
    );
    -- Partial unique index: deduplicate only when ref_id IS NOT NULL.
    -- NULLs are treated as distinct by SQLite UNIQUE, so this is the
    -- correct way to have optional idempotence.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_inbox_unique_ref
        ON agent_inbox(agent_id, kind, ref_id) WHERE ref_id IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_agent_inbox_agent_read
        ON agent_inbox(agent_id, read);
    CREATE INDEX IF NOT EXISTS idx_agent_inbox_created
        ON agent_inbox(created_at);
    -- Partial index: most rows have expires_at=NULL; this is smaller.
    CREATE INDEX IF NOT EXISTS idx_agent_inbox_expires
        ON agent_inbox(expires_at) WHERE expires_at IS NOT NULL;"];

/// SQLite-backed persistent store for agent-facing async notifications.
///
/// Each agent has its own row-set in this shared DB, partitioned by `agent_id`.
///
/// ## Idempotence
/// Writes that supply a non-NULL `ref_id` are deduplicated via the partial unique
/// index `idx_agent_inbox_unique_ref` (`UNIQUE(agent_id, kind, ref_id) WHERE ref_id IS NOT NULL`).
/// A second `write()` with the same `(agent_id, kind, ref_id)` returns `Ok(false)`.
/// Writes with `ref_id = None` skip idempotence checking and always produce a new row.
///
/// ## Threading
/// All public methods are `async` and must be awaited from an async context.
/// `StoreHandle` dispatches all DB work to `spawn_blocking` — never blocking the
/// Tokio runtime thread.
pub struct AgentInbox {
    store: StoreHandle,
    pub max_per_agent: usize,
}

impl AgentInbox {
    pub fn new(path: &Path, max_per_agent: usize) -> Result<Self, AgentOSError> {
        let conn = rusqlite::Connection::open(path).map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentInbox: open failed at {}: {e}", path.display()),
        })?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             PRAGMA temp_store=MEMORY;",
        )
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentInbox: PRAGMA failed: {e}"),
        })?;

        // Apply migrations inline (sync path).
        for (idx, migration) in MIGRATIONS.iter().enumerate() {
            let current_version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentInbox: user_version query failed: {e}"),
                })?;
            if (current_version as usize) <= idx {
                conn.execute_batch(migration)
                    .map_err(|e| AgentOSError::KernelError {
                        reason: format!("AgentInbox: migration {idx} failed: {e}"),
                    })?;
                let new_version = idx + 1;
                conn.execute_batch(&format!("PRAGMA user_version = {new_version}"))
                    .map_err(|e| AgentOSError::KernelError {
                        reason: format!("AgentInbox: set user_version failed: {e}"),
                    })?;
            }
        }

        Ok(Self {
            store: StoreHandle::from_conn(conn),
            max_per_agent,
        })
    }

    /// Idempotent insert when `ref_id` is `Some`. Returns `Ok(true)` on first
    /// write, `Ok(false)` when the unique index prevents a duplicate.
    /// When `ref_id` is `None`, always inserts (no deduplication).
    pub async fn write(&self, entry: &AgentInboxEntry) -> Result<bool, AgentOSError> {
        let entry = entry.clone();
        let max = self.max_per_agent;

        self.store
            .exec_mut(move |conn| {
                // Wrap count + evict + insert in a transaction to prevent a concurrent
                // writer from racing between the COUNT and the INSERT.
                let tx = conn.unchecked_transaction()?;

                // Cap-based eviction: keep oldest-read entries (read=1 DESC = evicted first),
                // then oldest-unread. Evict exactly (count - max + 1) rows, capped at PURGE_BATCH.
                let count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = ?1",
                    params![entry.agent_id.to_string()],
                    |r| r.get(0),
                )?;

                if count as usize >= max {
                    let to_evict = (count - max as i64 + 1).max(1).min(PURGE_BATCH as i64);
                    if let Err(e) = tx.execute(
                        "DELETE FROM agent_inbox WHERE id IN (
                             SELECT id FROM agent_inbox
                             WHERE agent_id = ?1
                             ORDER BY read DESC, created_at ASC
                             LIMIT ?2
                         )",
                        params![entry.agent_id.to_string(), to_evict],
                    ) {
                        tracing::warn!(
                            error = %e,
                            agent_id = %entry.agent_id,
                            "AgentInbox: cap eviction failed"
                        );
                        return Err(e);
                    }
                }

                let body_json = serde_json::to_string(&entry.body)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO agent_inbox
                     (id, agent_id, kind, title, body, ref_id, created_at, expires_at, read)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        entry.id.to_string(),
                        entry.agent_id.to_string(),
                        entry.kind.as_str(),
                        &entry.title,
                        body_json,
                        entry.ref_id.as_deref(),
                        entry.created_at.to_rfc3339(),
                        entry.expires_at.map(|d| d.to_rfc3339()),
                        if entry.read { 1i32 } else { 0i32 },
                    ],
                )?;

                tx.commit()?;
                Ok(inserted > 0)
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentInbox: write failed: {e}"),
            })
    }

    /// Cheap unread count used by the prompt renderer.
    pub async fn unread_count(&self, agent_id: AgentID) -> Result<u32, AgentOSError> {
        self.store
            .exec(move |conn| {
                let c: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM agent_inbox WHERE agent_id = ?1 AND read = 0",
                    params![agent_id.to_string()],
                    |r| r.get(0),
                )?;
                Ok(c.max(0) as u32)
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentInbox: unread_count failed: {e}"),
            })
    }

    /// List inbox entries for an agent. Returns titles but not bodies.
    pub async fn list(
        &self,
        agent_id: AgentID,
        unread_only: bool,
        limit: u32,
    ) -> Result<Vec<AgentInboxEntry>, AgentOSError> {
        self.store
            .exec(move |conn| {
                let sql = if unread_only {
                    "SELECT id, agent_id, kind, title, body, ref_id, created_at, expires_at, read
                       FROM agent_inbox
                      WHERE agent_id = ?1 AND read = 0
                      ORDER BY created_at DESC
                      LIMIT ?2"
                } else {
                    "SELECT id, agent_id, kind, title, body, ref_id, created_at, expires_at, read
                       FROM agent_inbox
                      WHERE agent_id = ?1
                      ORDER BY created_at DESC
                      LIMIT ?2"
                };
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(params![agent_id.to_string(), limit as i64], map_row)?;
                collect_rows(rows)
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentInbox: list failed: {e}"),
            })
    }

    /// Fetch a single entry by ID.
    pub async fn get(
        &self,
        id: AgentInboxEntryID,
    ) -> Result<Option<AgentInboxEntry>, AgentOSError> {
        self.store
            .exec(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, agent_id, kind, title, body, ref_id, created_at, expires_at, read
                       FROM agent_inbox WHERE id = ?1",
                )?;
                let mut rows = stmt.query_map(params![id.to_string()], map_row)?;
                match rows.next() {
                    None => Ok(None),
                    Some(r) => r.map(Some),
                }
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentInbox: get failed: {e}"),
            })
    }

    /// Mark a single entry as read. Returns an error if the ID is not found.
    pub async fn mark_read(&self, id: AgentInboxEntryID) -> Result<(), AgentOSError> {
        self.store
            .exec_mut(move |conn| {
                let n = conn.execute(
                    "UPDATE agent_inbox SET read = 1 WHERE id = ?1",
                    params![id.to_string()],
                )?;
                if n == 0 {
                    return Err(rusqlite::Error::QueryReturnedNoRows);
                }
                Ok(())
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentInbox: mark_read failed for {id}: {e}"),
            })
    }

    /// Delete an entry (dismiss).
    pub async fn dismiss(&self, id: AgentInboxEntryID) -> Result<(), AgentOSError> {
        self.store
            .exec_mut(move |conn| {
                conn.execute(
                    "DELETE FROM agent_inbox WHERE id = ?1",
                    params![id.to_string()],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentInbox: dismiss failed: {e}"),
            })
    }

    /// Delete every inbox entry belonging to `agent_id`. Returns the number of rows removed.
    pub async fn delete_all_for_agent(&self, agent_id: AgentID) -> Result<u32, AgentOSError> {
        self.store
            .exec_mut(move |conn| {
                let n = conn.execute(
                    "DELETE FROM agent_inbox WHERE agent_id = ?1",
                    params![agent_id.to_string()],
                )?;
                Ok(n as u32)
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentInbox: delete_all_for_agent failed: {e}"),
            })
    }

    /// Delete TTL-expired rows and re-enforce the per-agent cap.
    ///
    /// Called by `TimeoutChecker` every 10 minutes. Returns the number of rows
    /// deleted. The cap sweep uses `ORDER BY read ASC, created_at DESC` so that
    /// unread entries are preserved first; within each read-tier the newest entries
    /// survive. Read entries are the first candidates for eviction.
    pub async fn sweep_expired(&self) -> Result<u32, AgentOSError> {
        let max_per_agent = self.max_per_agent as i64;
        self.store
            .exec_mut(move |conn| {
                let now = Utc::now().to_rfc3339();

                let ttl_deleted = conn
                    .execute(
                        "DELETE FROM agent_inbox
                      WHERE expires_at IS NOT NULL AND expires_at < ?1",
                        params![now],
                    )
                    .map_err(|e| {
                        tracing::warn!(error = %e, "AgentInbox: TTL sweep DELETE failed");
                        e
                    })?;

                // Cap enforcement: keep only the most recent `max_per_agent` rows per
                // agent. ORDER: read ASC (unread=0 first, read=1 last) → unread
                // entries are preserved; created_at DESC → newest rows survive within
                // each tier. Any row with rn > max is evicted.
                let cap_deleted = conn
                    .execute(
                        "DELETE FROM agent_inbox
                      WHERE id IN (
                          SELECT id FROM (
                              SELECT id,
                                     ROW_NUMBER() OVER (
                                         PARTITION BY agent_id
                                         ORDER BY read ASC, created_at DESC
                                     ) AS rn
                              FROM agent_inbox
                          )
                          WHERE rn > ?1
                      )",
                        params![max_per_agent],
                    )
                    .map_err(|e| {
                        tracing::warn!(error = %e, "AgentInbox: cap sweep DELETE failed");
                        e
                    })?;

                Ok((ttl_deleted + cap_deleted) as u32)
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentInbox: sweep_expired failed: {e}"),
            })
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentInboxEntry> {
    let id_str: String = row.get(0)?;
    let agent_id_str: String = row.get(1)?;
    let kind_str: String = row.get(2)?;
    let body_str: String = row.get(4)?;
    let created_str: String = row.get(6)?;
    let expires_str: Option<String> = row.get(7)?;

    let id = id_str.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::fmt::Error),
        )
    })?;
    let agent_id = agent_id_str.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::fmt::Error),
        )
    })?;
    let kind = kind_str.parse().unwrap_or(AgentInboxKind::Scheduled);
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_str)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::fmt::Error),
            )
        })?;
    let expires_at = expires_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(std::fmt::Error),
                    )
                })
        })
        .transpose()?;

    Ok(AgentInboxEntry {
        id,
        agent_id,
        kind,
        title: row.get(3)?,
        body: serde_json::from_str(&body_str).unwrap_or(serde_json::Value::Null),
        ref_id: row.get(5)?,
        created_at,
        expires_at,
        read: row.get::<_, i32>(8)? != 0,
    })
}

fn collect_rows(
    rows: impl Iterator<Item = rusqlite::Result<AgentInboxEntry>>,
) -> rusqlite::Result<Vec<AgentInboxEntry>> {
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::AgentInboxEntryID;
    use tempfile::TempDir;

    fn make_entry(
        agent_id: AgentID,
        kind: AgentInboxKind,
        ref_id: Option<&str>,
    ) -> AgentInboxEntry {
        AgentInboxEntry {
            id: AgentInboxEntryID::new(),
            agent_id,
            kind,
            title: "test notification".into(),
            body: serde_json::json!({ "task_id": "t-1" }),
            ref_id: ref_id.map(String::from),
            created_at: Utc::now(),
            expires_at: None,
            read: false,
        }
    }

    #[tokio::test]
    async fn write_and_count_unread() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let agent = AgentID::new();
        let entry = make_entry(agent, AgentInboxKind::Scheduled, Some("task-1"));

        assert!(inbox.write(&entry).await.unwrap(), "first write inserts");
        assert!(
            !inbox.write(&entry).await.unwrap(),
            "duplicate returns false"
        );
        assert_eq!(inbox.unread_count(agent).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn write_with_null_ref_id_does_not_deduplicate() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let agent = AgentID::new();

        // Two distinct entries with ref_id=None are both inserted (no idempotence).
        let e1 = make_entry(agent, AgentInboxKind::Event, None);
        let e2 = make_entry(agent, AgentInboxKind::Event, None);
        assert!(inbox.write(&e1).await.unwrap());
        assert!(inbox.write(&e2).await.unwrap());
        assert_eq!(inbox.unread_count(agent).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn list_returns_entries_ordered_by_created_desc() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let agent = AgentID::new();

        for i in 0..3u8 {
            let mut e = make_entry(agent, AgentInboxKind::Event, Some(&format!("ev-{i}")));
            e.created_at = Utc::now() + chrono::Duration::seconds(i as i64);
            inbox.write(&e).await.unwrap();
        }

        let list = inbox.list(agent, false, 10).await.unwrap();
        assert_eq!(list.len(), 3);
        assert!(list[0].created_at >= list[1].created_at);
    }

    #[tokio::test]
    async fn mark_read_decrements_unread_count() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let agent = AgentID::new();
        let entry = make_entry(agent, AgentInboxKind::Scheduled, Some("task-2"));
        inbox.write(&entry).await.unwrap();

        assert_eq!(inbox.unread_count(agent).await.unwrap(), 1);
        inbox.mark_read(entry.id).await.unwrap();
        assert_eq!(inbox.unread_count(agent).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn cap_evicts_oldest_read_first() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentInbox::new(&tmp.path().join("t.db"), 3).unwrap();
        let agent = AgentID::new();

        // Write 3 entries, mark first 2 as read.
        let mut ids = Vec::new();
        for i in 0..3u8 {
            let e = make_entry(agent, AgentInboxKind::Timer, Some(&format!("t-{i}")));
            ids.push(e.id);
            inbox.write(&e).await.unwrap();
        }
        inbox.mark_read(ids[0]).await.unwrap();
        inbox.mark_read(ids[1]).await.unwrap();

        // 4th write: cap=3, evict exactly 1 read entry to make room.
        let e4 = make_entry(agent, AgentInboxKind::Scheduled, Some("task-x"));
        inbox.write(&e4).await.unwrap();

        // Unread count should be 2 (ids[2] + e4); one of the read entries was evicted.
        assert_eq!(inbox.unread_count(agent).await.unwrap(), 2);
        // The unread entry ids[2] must survive.
        assert!(
            inbox.get(ids[2]).await.unwrap().is_some(),
            "unread entry must survive"
        );
    }

    #[tokio::test]
    async fn sweep_expired_removes_ttl_entries() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let agent = AgentID::new();

        let mut entry = make_entry(agent, AgentInboxKind::Event, Some("old-event"));
        entry.expires_at = Some(Utc::now() - chrono::Duration::days(1));
        inbox.write(&entry).await.unwrap();

        let pruned = inbox.sweep_expired().await.unwrap();
        assert!(pruned >= 1, "should have pruned the expired entry");
        assert_eq!(inbox.unread_count(agent).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn sweep_cap_enforces_and_preserves_unread() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("t.db");
        let agent = AgentID::new();

        // Phase 1: write 5 entries using a large-cap inbox to bypass write-time eviction.
        let wide = AgentInbox::new(&db_path, 1000).unwrap();
        let mut read_ids = Vec::new();
        let mut unread_ids = Vec::new();
        for i in 0..5u8 {
            let e = make_entry(agent, AgentInboxKind::Timer, Some(&format!("entry-{i}")));
            let id = e.id;
            wide.write(&e).await.unwrap();
            if i < 3 {
                wide.mark_read(id).await.unwrap();
                read_ids.push(id);
            } else {
                unread_ids.push(id);
            }
        }

        // Phase 2: open the same DB with cap=3 to simulate a config reduction,
        // then call sweep. Exactly 2 entries (oldest read) should be evicted.
        let narrow = AgentInbox::new(&db_path, 3).unwrap();
        let pruned = narrow.sweep_expired().await.unwrap();
        assert!(pruned >= 2, "sweep should evict overage; got {pruned}");

        // Unread entries must survive.
        for uid in &unread_ids {
            assert!(
                narrow.get(*uid).await.unwrap().is_some(),
                "unread entry {uid} must survive cap sweep"
            );
        }
        assert_eq!(narrow.unread_count(agent).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn cross_agent_isolation() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let alice = AgentID::new();
        let bob = AgentID::new();

        inbox
            .write(&make_entry(
                alice,
                AgentInboxKind::Scheduled,
                Some("t-alice"),
            ))
            .await
            .unwrap();

        assert_eq!(inbox.unread_count(bob).await.unwrap(), 0);
        assert!(inbox.list(bob, false, 10).await.unwrap().is_empty());
    }
}
