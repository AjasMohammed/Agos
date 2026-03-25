use agentos_types::{
    AgentOSError, DeliveryChannel, DeliveryStatus, NotificationID, NotificationPriority,
    UserMessage, UserResponse,
};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Number of oldest read messages to delete when the inbox exceeds `max_inbox_size`.
const PURGE_BATCH: usize = 100;

/// SQLite-backed persistent store for user-directed notifications.
///
/// All async methods acquire a `tokio::sync::Mutex` around the `rusqlite::Connection`
/// and then call `spawn_blocking` so the synchronous SQLite I/O never blocks the
/// Tokio thread pool (Architecture Review GAP-5).
pub struct UserInbox {
    db: Arc<Mutex<Connection>>,
    max_inbox_size: usize,
}

impl UserInbox {
    /// Open (or create) the inbox database at `db_path`.
    ///
    /// `max_inbox_size` controls how many messages are kept before the oldest
    /// read messages are purged (defaults to 1000 via `KernelConfig`).
    pub fn new(db_path: &Path, max_inbox_size: usize) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path).map_err(|e| AgentOSError::KernelError {
            reason: format!(
                "UserInbox: failed to open DB at {}: {}",
                db_path.display(),
                e
            ),
        })?;

        // Enable WAL mode for concurrent readers.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: PRAGMA failed: {e}"),
            })?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS user_messages (
                id               TEXT PRIMARY KEY,
                from_source      TEXT NOT NULL,
                task_id          TEXT,
                trace_id         TEXT NOT NULL,
                kind             TEXT NOT NULL,
                priority         TEXT NOT NULL,
                subject          TEXT NOT NULL,
                body             TEXT NOT NULL,
                interaction      TEXT,
                delivery_status  TEXT NOT NULL DEFAULT '{}',
                response         TEXT,
                created_at       TEXT NOT NULL,
                expires_at       TEXT,
                read             INTEGER NOT NULL DEFAULT 0,
                thread_id        TEXT,
                reply_to_external_id TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_um_created_at ON user_messages(created_at);
            CREATE INDEX IF NOT EXISTS idx_um_read ON user_messages(read);
            CREATE INDEX IF NOT EXISTS idx_um_thread_id ON user_messages(thread_id);",
        )
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: schema creation failed: {e}"),
        })?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            max_inbox_size,
        })
    }

    /// Persist a new `UserMessage` to the inbox.
    ///
    /// If the inbox would exceed `MAX_INBOX_SIZE` after this insert, the oldest
    /// `PURGE_BATCH` read messages are deleted first.
    pub async fn write(&self, msg: &UserMessage) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let max_size = self.max_inbox_size;
        let msg = msg.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            // Purge oldest read messages if inbox is at capacity.
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM user_messages", [], |r| r.get(0))
                .unwrap_or(0);
            if count as usize >= max_size {
                conn.execute(
                    "DELETE FROM user_messages WHERE id IN (
                         SELECT id FROM user_messages WHERE read = 1
                         ORDER BY created_at ASC LIMIT ?1
                     )",
                    params![PURGE_BATCH as i64],
                )
                .ok();
            }

            let from_json =
                serde_json::to_string(&msg.from).map_err(|e| AgentOSError::KernelError {
                    reason: format!("UserInbox: failed to serialize from: {e}"),
                })?;
            let kind_json =
                serde_json::to_string(&msg.kind).map_err(|e| AgentOSError::KernelError {
                    reason: format!("UserInbox: failed to serialize kind: {e}"),
                })?;
            let interaction_json = msg
                .interaction
                .as_ref()
                .and_then(|i| serde_json::to_string(i).ok());
            let delivery_json =
                serde_json::to_string(&msg.delivery_status).unwrap_or_else(|_| "{}".into());
            let response_json = msg
                .response
                .as_ref()
                .and_then(|r| serde_json::to_string(r).ok());
            let expires_str = msg.expires_at.map(|d| d.to_rfc3339());

            conn.execute(
                "INSERT OR REPLACE INTO user_messages
                 (id, from_source, task_id, trace_id, kind, priority, subject, body,
                  interaction, delivery_status, response, created_at, expires_at, read,
                  thread_id, reply_to_external_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![
                    msg.id.to_string(),
                    from_json,
                    msg.task_id.map(|t| t.to_string()),
                    msg.trace_id.to_string(),
                    kind_json,
                    msg.priority.to_string(),
                    msg.subject,
                    msg.body,
                    interaction_json,
                    delivery_json,
                    response_json,
                    msg.created_at.to_rfc3339(),
                    expires_str,
                    msg.read as i32,
                    msg.thread_id,
                    msg.reply_to_external_id,
                ],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: insert failed: {e}"),
            })?;
            Ok::<_, AgentOSError>(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: task join error: {e}"),
        })??;
        Ok(())
    }

    /// Update the delivery status for a specific channel on a message.
    pub async fn update_delivery_status(
        &self,
        id: &NotificationID,
        channel: DeliveryChannel,
        status: DeliveryStatus,
    ) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let id_str = id.to_string();
        let channel_key = channel.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            // Load current delivery_status JSON, merge the update, write back.
            let current: Option<String> = conn
                .query_row(
                    "SELECT delivery_status FROM user_messages WHERE id = ?1",
                    params![id_str],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            let mut map: std::collections::HashMap<String, serde_json::Value> = current
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            map.insert(
                channel_key,
                serde_json::to_value(&status).unwrap_or(serde_json::Value::Null),
            );
            let new_json = serde_json::to_string(&map).unwrap_or_else(|_| "{}".into());
            conn.execute(
                "UPDATE user_messages SET delivery_status = ?1 WHERE id = ?2",
                params![new_json, id_str],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: update_delivery_status failed: {e}"),
            })?;
            Ok::<_, AgentOSError>(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: task join error: {e}"),
        })??;
        Ok(())
    }

    /// Mark a notification as read by the user.
    pub async fn mark_read(&self, id: &NotificationID) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let id_str = id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            conn.execute(
                "UPDATE user_messages SET read = 1 WHERE id = ?1",
                params![id_str],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: mark_read failed: {e}"),
            })?;
            Ok::<_, AgentOSError>(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: task join error: {e}"),
        })??;
        Ok(())
    }

    /// Store a user response on an interactive notification.
    ///
    /// Uses `UPDATE … WHERE response IS NULL` so only the first caller succeeds;
    /// concurrent attempts (e.g. web UI and Telegram simultaneously) are rejected
    /// atomically at the SQLite level rather than via a read-then-write race.
    pub async fn set_response(
        &self,
        id: &NotificationID,
        response: &UserResponse,
    ) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let id_str = id.to_string();
        let resp_json = serde_json::to_string(response).map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: response serialisation failed: {e}"),
        })?;
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let changed = conn
                .execute(
                    "UPDATE user_messages SET response = ?1 WHERE id = ?2 AND response IS NULL",
                    params![resp_json, id_str],
                )
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("UserInbox: set_response failed: {e}"),
                })?;
            if changed == 0 {
                return Err(AgentOSError::KernelError {
                    reason: format!(
                        "UserInbox: notification {id_str} not found or already has a response"
                    ),
                });
            }
            Ok::<_, AgentOSError>(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: task join error: {e}"),
        })??;
        Ok(())
    }

    /// List messages from the inbox, ordered by creation time (newest first).
    pub async fn list(
        &self,
        unread_only: bool,
        limit: usize,
    ) -> Result<Vec<UserMessage>, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let sql = if unread_only {
                "SELECT id, from_source, task_id, trace_id, kind, priority, subject, body,
                        interaction, delivery_status, response, created_at, expires_at, read,
                        thread_id, reply_to_external_id
                 FROM user_messages WHERE read = 0
                 ORDER BY created_at DESC LIMIT ?1"
            } else {
                "SELECT id, from_source, task_id, trace_id, kind, priority, subject, body,
                        interaction, delivery_status, response, created_at, expires_at, read,
                        thread_id, reply_to_external_id
                 FROM user_messages
                 ORDER BY created_at DESC LIMIT ?1"
            };
            let mut stmt = conn.prepare(sql).map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: prepare failed: {e}"),
            })?;
            let rows = stmt
                .query_map(params![limit as i64], row_to_user_message)
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("UserInbox: query failed: {e}"),
                })?;
            let mut msgs = Vec::new();
            for row in rows {
                match row {
                    Ok(Ok(msg)) => msgs.push(msg),
                    Ok(Err(e)) => tracing::warn!("UserInbox: skipping malformed row: {}", e),
                    Err(e) => {
                        return Err(AgentOSError::KernelError {
                            reason: format!("UserInbox: row error: {e}"),
                        })
                    }
                }
            }
            Ok::<_, AgentOSError>(msgs)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: task join error: {e}"),
        })?
    }

    /// Fetch a single message by ID.
    pub async fn get(&self, id: &NotificationID) -> Result<Option<UserMessage>, AgentOSError> {
        let db = self.db.clone();
        let id_str = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<UserMessage>, AgentOSError> {
            let conn = db.blocking_lock();
            let result = conn.query_row(
                "SELECT id, from_source, task_id, trace_id, kind, priority, subject, body,
                        interaction, delivery_status, response, created_at, expires_at, read,
                        thread_id, reply_to_external_id
                 FROM user_messages WHERE id = ?1",
                params![id_str],
                row_to_user_message,
            );
            match result {
                Ok(Ok(msg)) => Ok(Some(msg)),
                Ok(Err(e)) => Err(AgentOSError::KernelError {
                    reason: format!("UserInbox: row deserialization failed: {e}"),
                }),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(AgentOSError::KernelError {
                    reason: format!("UserInbox: get failed: {e}"),
                }),
            }
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: task join error: {e}"),
        })?
    }

    /// Count unread messages using a single `COUNT(*)` query — used by the web bell counter.
    pub async fn count_unread(&self) -> usize {
        let db = self.db.clone();
        match tokio::task::spawn_blocking(move || -> Result<i64, rusqlite::Error> {
            let conn = db.blocking_lock();
            conn.query_row(
                "SELECT COUNT(*) FROM user_messages WHERE read = 0",
                [],
                |r| r.get(0),
            )
        })
        .await
        {
            Ok(Ok(n)) => n as usize,
            _ => 0,
        }
    }

    /// Return all unanswered interactive messages (blocking questions with no response).
    ///
    /// Used by `InboundRouter` to auto-route a reply when exactly one task is waiting.
    pub async fn list_pending_questions(&self) -> Vec<UserMessage> {
        let db = self.db.clone();
        match tokio::task::spawn_blocking(move || -> Result<Vec<UserMessage>, rusqlite::Error> {
            let conn = db.blocking_lock();
            let mut stmt = conn.prepare(
                "SELECT id, from_source, task_id, trace_id, kind, priority, subject, body,
                        interaction, delivery_status, response, created_at, expires_at, read,
                        thread_id, reply_to_external_id
                 FROM user_messages
                 WHERE response IS NULL AND interaction IS NOT NULL
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], row_to_user_message)?;
            Ok(rows.flatten().flatten().collect())
        })
        .await
        {
            Ok(Ok(msgs)) => msgs,
            Ok(Err(e)) => {
                tracing::warn!("UserInbox: list_pending_questions failed: {}", e);
                vec![]
            }
            Err(e) => {
                tracing::warn!("UserInbox: list_pending_questions task panicked: {}", e);
                vec![]
            }
        }
    }

    /// Return all un-responded Question messages whose `expires_at` is in the past.
    pub async fn list_expired_questions(&self, now: chrono::DateTime<Utc>) -> Vec<UserMessage> {
        let db = self.db.clone();
        let now_str = now.to_rfc3339();
        match tokio::task::spawn_blocking(move || -> Result<Vec<UserMessage>, rusqlite::Error> {
            let conn = db.blocking_lock();
            let mut stmt = conn.prepare(
                "SELECT id, from_source, task_id, trace_id, kind, priority, subject, body,
                        interaction, delivery_status, response, created_at, expires_at, read,
                        thread_id, reply_to_external_id
                 FROM user_messages
                 WHERE expires_at IS NOT NULL
                   AND expires_at < ?1
                   AND response IS NULL",
            )?;
            let rows = stmt.query_map(params![now_str], row_to_user_message)?;
            Ok(rows.flatten().flatten().collect())
        })
        .await
        {
            Ok(Ok(msgs)) => msgs,
            Ok(Err(e)) => {
                tracing::warn!("UserInbox: list_expired_questions query failed: {}", e);
                vec![]
            }
            Err(e) => {
                tracing::warn!("UserInbox: list_expired_questions task panicked: {}", e);
                vec![]
            }
        }
    }
}

/// Map a rusqlite `Row` to a `UserMessage`, deserialising JSON columns.
fn row_to_user_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<UserMessage, String>> {
    let id_str: String = row.get(0)?;
    let from_json: String = row.get(1)?;
    let task_id_str: Option<String> = row.get(2)?;
    let trace_id_str: String = row.get(3)?;
    let kind_json: String = row.get(4)?;
    let priority_str: String = row.get(5)?;
    let subject: String = row.get(6)?;
    let body: String = row.get(7)?;
    let interaction_json: Option<String> = row.get(8)?;
    let delivery_json: String = row.get(9)?;
    let response_json: Option<String> = row.get(10)?;
    let created_str: String = row.get(11)?;
    let expires_str: Option<String> = row.get(12)?;
    let read: i32 = row.get(13)?;
    let thread_id: Option<String> = row.get(14)?;
    let reply_to_external_id: Option<String> = row.get(15)?;

    macro_rules! deser {
        ($json:expr, $ty:ty) => {
            match serde_json::from_str::<$ty>(&$json) {
                Ok(v) => v,
                Err(e) => return Ok(Err(format!("deser error: {e}"))),
            }
        };
    }

    let id: NotificationID = match id_str.parse() {
        Ok(v) => v,
        Err(e) => return Ok(Err(format!("bad id: {e}"))),
    };
    let task_id = task_id_str.and_then(|s| s.parse().ok());
    let trace_id = match trace_id_str.parse() {
        Ok(v) => v,
        Err(e) => return Ok(Err(format!("bad trace_id: {e}"))),
    };
    let created_at = match chrono::DateTime::parse_from_rfc3339(&created_str) {
        Ok(d) => d.with_timezone(&Utc),
        Err(e) => return Ok(Err(format!("bad created_at: {e}"))),
    };
    let expires_at = expires_str.and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    });
    let priority: NotificationPriority = deser!(priority_str, NotificationPriority);
    let from = deser!(from_json, agentos_types::NotificationSource);
    let kind = deser!(kind_json, agentos_types::UserMessageKind);
    let interaction = interaction_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let delivery_status = serde_json::from_str(&delivery_json).unwrap_or_default();
    let response = response_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    Ok(Ok(UserMessage {
        id,
        from,
        task_id,
        trace_id,
        kind,
        priority,
        subject,
        body,
        interaction,
        delivery_status,
        response,
        created_at,
        expires_at,
        read: read != 0,
        thread_id,
        reply_to_external_id,
    }))
}
