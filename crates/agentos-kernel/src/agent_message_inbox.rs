use agentos_types::{AgentID, AgentMessageEntry, AgentMessageEntryID, AgentOSError};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

const PURGE_BATCH: usize = 32;

/// SQLite-backed persistent store for agent-to-agent direct messages.
///
/// Written by the `send-agent-message` tool before the in-memory fan-out so
/// the message survives agent restart. The `unread_by_sender` query is used
/// by the `InboxPromptRenderer` to produce a stable, per-sender count line.
///
/// ## Threading
/// All public methods are `async` and must be awaited from an async context.
/// `blocking_lock()` is only called inside `spawn_blocking` closures.
pub struct AgentMessageInbox {
    db: Arc<Mutex<Connection>>,
    pub max_per_agent: usize,
}

impl AgentMessageInbox {
    pub fn new(path: &Path, max_per_agent: usize) -> Result<Self, AgentOSError> {
        let conn = Connection::open(path).map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: open failed at {}: {e}", path.display()),
        })?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentMessageInbox: PRAGMA failed: {e}"),
            })?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_messages (
                id               TEXT PRIMARY KEY,
                from_agent_id    TEXT NOT NULL,
                from_agent_name  TEXT NOT NULL,
                to_agent_id      TEXT NOT NULL,
                body             TEXT NOT NULL,
                reply_to         TEXT,
                created_at       TEXT NOT NULL,
                expires_at       TEXT,
                read             INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_am_to_read
                ON agent_messages(to_agent_id, read);
            CREATE INDEX IF NOT EXISTS idx_am_from
                ON agent_messages(from_agent_id);
            CREATE INDEX IF NOT EXISTS idx_am_created
                ON agent_messages(created_at);
            -- Partial index: most messages have no expiry.
            CREATE INDEX IF NOT EXISTS idx_am_expires
                ON agent_messages(expires_at) WHERE expires_at IS NOT NULL;",
        )
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: schema init failed: {e}"),
        })?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            max_per_agent,
        })
    }

    /// Persist a new message. Returns `Ok(true)` on insert, `Ok(false)` if the
    /// primary key already exists (no duplicate created).
    pub async fn write(&self, entry: &AgentMessageEntry) -> Result<bool, AgentOSError> {
        let db = self.db.clone();
        let entry = entry.clone();
        let max = self.max_per_agent;

        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();

            let tx = conn
                .unchecked_transaction()
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: begin tx: {e}"),
                })?;

            let count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM agent_messages WHERE to_agent_id = ?1",
                    params![entry.to_agent_id.to_string()],
                    |r| r.get(0),
                )
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: COUNT failed: {e}"),
                })?;

            if count as usize >= max {
                let to_evict = (count - max as i64 + 1).max(1).min(PURGE_BATCH as i64);
                tx.execute(
                    "DELETE FROM agent_messages WHERE id IN (
                         SELECT id FROM agent_messages
                         WHERE to_agent_id = ?1
                         ORDER BY read DESC, created_at ASC
                         LIMIT ?2
                     )",
                    params![entry.to_agent_id.to_string(), to_evict],
                )
                .map_err(|e| {
                    tracing::warn!(
                        error = %e,
                        to_agent_id = %entry.to_agent_id,
                        "AgentMessageInbox: cap eviction failed"
                    );
                    AgentOSError::KernelError {
                        reason: format!("AgentMessageInbox: eviction failed: {e}"),
                    }
                })?;
            }

            let inserted = tx
                .execute(
                    "INSERT OR IGNORE INTO agent_messages
                     (id, from_agent_id, from_agent_name, to_agent_id, body,
                      reply_to, created_at, expires_at, read)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        entry.id.to_string(),
                        entry.from_agent_id.to_string(),
                        &entry.from_agent_name,
                        entry.to_agent_id.to_string(),
                        &entry.body,
                        entry.reply_to.map(|id| id.to_string()),
                        entry.created_at.to_rfc3339(),
                        entry.expires_at.map(|d| d.to_rfc3339()),
                        if entry.read { 1i32 } else { 0i32 },
                    ],
                )
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: insert failed: {e}"),
                })?;

            tx.commit().map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentMessageInbox: commit: {e}"),
            })?;

            Ok(inserted > 0)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: spawn_blocking join: {e}"),
        })?
    }

    /// Per-sender unread counts used by the `InboxPromptRenderer`.
    ///
    /// Returns `Vec<(from_agent_id, from_agent_name, count)>` ordered by
    /// `count DESC, from_agent_name ASC` for stable prompt-cache output.
    pub async fn unread_by_sender(
        &self,
        to_agent_id: AgentID,
    ) -> Result<Vec<(AgentID, String, u32)>, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT from_agent_id,
                            MAX(from_agent_name) AS from_agent_name,
                            COUNT(*) AS c
                       FROM agent_messages
                      WHERE to_agent_id = ?1 AND read = 0
                      GROUP BY from_agent_id
                      ORDER BY c DESC, from_agent_name ASC",
                )
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: prepare unread_by_sender: {e}"),
                })?;

            let rows = stmt
                .query_map(params![to_agent_id.to_string()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: query unread_by_sender: {e}"),
                })?;

            let mut out = Vec::new();
            for r in rows {
                let (aid_str, name, c) = r.map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: row unread_by_sender: {e}"),
                })?;
                let agent_id: AgentID = aid_str.parse().map_err(|_| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: invalid agent_id: {aid_str}"),
                })?;
                out.push((agent_id, name, c.max(0) as u32));
            }
            Ok(out)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: unread_by_sender join: {e}"),
        })?
    }

    /// Total unread message count for an agent.
    pub async fn unread_count(&self, agent_id: AgentID) -> Result<u32, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let c: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM agent_messages WHERE to_agent_id = ?1 AND read = 0",
                    params![agent_id.to_string()],
                    |r| r.get(0),
                )
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: unread_count query failed: {e}"),
                })?;
            Ok(c.max(0) as u32)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: unread_count join: {e}"),
        })?
    }

    /// List messages addressed to `agent_id`.
    pub async fn list(
        &self,
        agent_id: AgentID,
        unread_only: bool,
        limit: u32,
    ) -> Result<Vec<AgentMessageEntry>, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let sql = if unread_only {
                "SELECT id, from_agent_id, from_agent_name, to_agent_id, body,
                        reply_to, created_at, expires_at, read
                   FROM agent_messages
                  WHERE to_agent_id = ?1 AND read = 0
                  ORDER BY created_at DESC
                  LIMIT ?2"
            } else {
                "SELECT id, from_agent_id, from_agent_name, to_agent_id, body,
                        reply_to, created_at, expires_at, read
                   FROM agent_messages
                  WHERE to_agent_id = ?1
                  ORDER BY created_at DESC
                  LIMIT ?2"
            };
            let mut stmt = conn.prepare(sql).map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentMessageInbox: prepare list: {e}"),
            })?;
            let rows = stmt
                .query_map(params![agent_id.to_string(), limit as i64], map_row)
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: query list: {e}"),
                })?;
            collect_rows(rows)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: list join: {e}"),
        })?
    }

    /// Fetch a single message by ID.
    pub async fn get(
        &self,
        id: AgentMessageEntryID,
    ) -> Result<Option<AgentMessageEntry>, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, from_agent_id, from_agent_name, to_agent_id, body,
                            reply_to, created_at, expires_at, read
                       FROM agent_messages WHERE id = ?1",
                )
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: prepare get: {e}"),
                })?;
            let mut rows = stmt
                .query_map(params![id.to_string()], map_row)
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: query get: {e}"),
                })?;
            match rows.next() {
                None => Ok(None),
                Some(r) => r.map(Some).map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: row get: {e}"),
                }),
            }
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: get join: {e}"),
        })?
    }

    /// Mark a message as read.
    pub async fn mark_read(&self, id: AgentMessageEntryID) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let n = conn
                .execute(
                    "UPDATE agent_messages SET read = 1 WHERE id = ?1",
                    params![id.to_string()],
                )
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: mark_read: {e}"),
                })?;
            if n == 0 {
                return Err(AgentOSError::KernelError {
                    reason: format!("AgentMessageInbox: entry {id} not found"),
                });
            }
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: mark_read join: {e}"),
        })?
    }

    /// Delete a message.
    pub async fn dismiss(&self, id: AgentMessageEntryID) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            conn.execute(
                "DELETE FROM agent_messages WHERE id = ?1",
                params![id.to_string()],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("AgentMessageInbox: dismiss: {e}"),
            })?;
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: dismiss join: {e}"),
        })?
    }

    /// Delete TTL-expired rows and re-enforce the per-agent cap.
    ///
    /// Called by `TimeoutChecker` every 10 minutes. Returns the number of rows
    /// deleted. The cap sweep preserves unread entries first, then newest read
    /// entries. Oldest read entries are evicted first.
    pub async fn sweep_expired(&self) -> Result<u32, AgentOSError> {
        let db = self.db.clone();
        let max_per_agent = self.max_per_agent as i64;
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let now = Utc::now().to_rfc3339();

            let ttl_deleted = conn
                .execute(
                    "DELETE FROM agent_messages
                      WHERE expires_at IS NOT NULL AND expires_at < ?1",
                    params![now],
                )
                .map_err(|e| {
                    tracing::warn!(error = %e, "AgentMessageInbox: TTL sweep DELETE failed");
                    AgentOSError::KernelError {
                        reason: format!("AgentMessageInbox: TTL sweep: {e}"),
                    }
                })?;

            // Cap enforcement: keep unread first (read ASC), newest first within
            // each tier (created_at DESC). Rows with rn > max are evicted.
            let cap_deleted = conn
                .execute(
                    "DELETE FROM agent_messages
                      WHERE id IN (
                          SELECT id FROM (
                              SELECT id,
                                     ROW_NUMBER() OVER (
                                         PARTITION BY to_agent_id
                                         ORDER BY read ASC, created_at DESC
                                     ) AS rn
                              FROM agent_messages
                          )
                          WHERE rn > ?1
                      )",
                    params![max_per_agent],
                )
                .map_err(|e| {
                    tracing::warn!(error = %e, "AgentMessageInbox: cap sweep DELETE failed");
                    AgentOSError::KernelError {
                        reason: format!("AgentMessageInbox: cap sweep: {e}"),
                    }
                })?;

            Ok((ttl_deleted + cap_deleted) as u32)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: sweep join: {e}"),
        })?
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMessageEntry> {
    let id_str: String = row.get(0)?;
    let from_id_str: String = row.get(1)?;
    let to_id_str: String = row.get(3)?;
    let created_str: String = row.get(6)?;
    let expires_str: Option<String> = row.get(7)?;
    let reply_to_str: Option<String> = row.get(5)?;

    let id = id_str.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::fmt::Error),
        )
    })?;
    let from_agent_id = from_id_str.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::fmt::Error),
        )
    })?;
    let to_agent_id = to_id_str.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::fmt::Error),
        )
    })?;
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

    let reply_to = reply_to_str
        .map(|s| {
            s.parse().map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(std::fmt::Error),
                )
            })
        })
        .transpose()?;

    Ok(AgentMessageEntry {
        id,
        from_agent_id,
        from_agent_name: row.get(2)?,
        to_agent_id,
        body: row.get(4)?,
        reply_to,
        created_at,
        expires_at,
        read: row.get::<_, i32>(8)? != 0,
    })
}

fn collect_rows(
    rows: impl Iterator<Item = rusqlite::Result<AgentMessageEntry>>,
) -> Result<Vec<AgentMessageEntry>, AgentOSError> {
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| AgentOSError::KernelError {
            reason: format!("AgentMessageInbox: row decode: {e}"),
        })?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::AgentMessageEntryID;
    use tempfile::TempDir;

    fn make_entry(from: AgentID, from_name: &str, to: AgentID, body: &str) -> AgentMessageEntry {
        AgentMessageEntry {
            id: AgentMessageEntryID::new(),
            from_agent_id: from,
            from_agent_name: from_name.to_string(),
            to_agent_id: to,
            body: body.to_string(),
            reply_to: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
        }
    }

    #[tokio::test]
    async fn write_and_unread_count() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentMessageInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let alice = AgentID::new();
        let bob = AgentID::new();

        let entry = make_entry(alice, "alice", bob, "hello bob");
        assert!(inbox.write(&entry).await.unwrap());
        // Duplicate primary key → not inserted.
        assert!(!inbox.write(&entry).await.unwrap());

        assert_eq!(inbox.unread_count(bob).await.unwrap(), 1);
        assert_eq!(inbox.unread_count(alice).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn unread_by_sender_groups_and_orders() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentMessageInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let bob = AgentID::new();
        let alice = AgentID::new();
        let charlie = AgentID::new();

        // 2 from alice, 3 from charlie
        for i in 0..2u8 {
            inbox
                .write(&make_entry(alice, "alice", bob, &format!("msg-alice-{i}")))
                .await
                .unwrap();
        }
        for i in 0..3u8 {
            inbox
                .write(&make_entry(
                    charlie,
                    "charlie",
                    bob,
                    &format!("msg-charlie-{i}"),
                ))
                .await
                .unwrap();
        }

        let by_sender = inbox.unread_by_sender(bob).await.unwrap();
        assert_eq!(by_sender.len(), 2);
        assert_eq!(by_sender[0].1, "charlie");
        assert_eq!(by_sender[0].2, 3);
        assert_eq!(by_sender[1].1, "alice");
        assert_eq!(by_sender[1].2, 2);
    }

    #[tokio::test]
    async fn mark_read_reduces_unread() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentMessageInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let alice = AgentID::new();
        let bob = AgentID::new();
        let e = make_entry(alice, "alice", bob, "hi");
        inbox.write(&e).await.unwrap();

        inbox.mark_read(e.id).await.unwrap();
        assert_eq!(inbox.unread_count(bob).await.unwrap(), 0);

        let by_sender = inbox.unread_by_sender(bob).await.unwrap();
        assert!(by_sender.is_empty());
    }

    #[tokio::test]
    async fn sweep_expired_removes_old_messages() {
        let tmp = TempDir::new().unwrap();
        let inbox = AgentMessageInbox::new(&tmp.path().join("t.db"), 200).unwrap();
        let alice = AgentID::new();
        let bob = AgentID::new();

        let mut e = make_entry(alice, "alice", bob, "stale");
        e.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        inbox.write(&e).await.unwrap();

        let pruned = inbox.sweep_expired().await.unwrap();
        assert!(pruned >= 1);
        assert_eq!(inbox.unread_count(bob).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn sweep_cap_preserves_unread_over_read() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("t.db");
        let alice = AgentID::new();
        let bob = AgentID::new();

        // Phase 1: write 5 messages using a large-cap inbox to bypass write-time eviction.
        let wide = AgentMessageInbox::new(&db_path, 1000).unwrap();
        let mut unread_ids = Vec::new();
        for i in 0..5u8 {
            let e = make_entry(alice, "alice", bob, &format!("msg-{i}"));
            let id = e.id;
            wide.write(&e).await.unwrap();
            if i < 3 {
                wide.mark_read(id).await.unwrap();
            } else {
                unread_ids.push(id);
            }
        }

        // Phase 2: open same DB with cap=3 to simulate config reduction, sweep.
        let narrow = AgentMessageInbox::new(&db_path, 3).unwrap();
        let pruned = narrow.sweep_expired().await.unwrap();
        assert!(pruned >= 2, "expected >= 2 pruned, got {pruned}");

        // Unread messages must survive.
        for uid in &unread_ids {
            assert!(
                narrow.get(*uid).await.unwrap().is_some(),
                "unread message {uid} must survive cap sweep"
            );
        }
        assert_eq!(narrow.unread_count(bob).await.unwrap(), 2);
    }
}
