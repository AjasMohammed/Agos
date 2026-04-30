use agentos_storage::{Migrations, StoreHandle};
use agentos_types::{
    AgentOSError, DeliveryChannel, DeliveryStatus, NotificationID, NotificationPriority,
    UserMessage, UserResponse,
};
use chrono::Utc;
use rusqlite::params;
use std::path::Path;

/// Number of oldest read messages to delete when the inbox exceeds `max_inbox_size`.
const PURGE_BATCH: usize = 100;

const MIGRATIONS: Migrations = &["CREATE TABLE IF NOT EXISTS user_messages (
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
    CREATE INDEX IF NOT EXISTS idx_um_thread_id ON user_messages(thread_id);"];

/// SQLite-backed persistent store for user-directed notifications.
///
/// All async methods dispatch to `StoreHandle` which uses `spawn_blocking`
/// internally so the synchronous SQLite I/O never blocks the Tokio thread pool.
pub struct UserInbox {
    store: StoreHandle,
    max_inbox_size: usize,
}

impl UserInbox {
    /// Open (or create) the inbox database at `db_path`.
    ///
    /// `max_inbox_size` controls how many messages are kept before the oldest
    /// read messages are purged (defaults to 1000 via `KernelConfig`).
    pub fn new(db_path: &Path, max_inbox_size: usize) -> Result<Self, AgentOSError> {
        let conn = rusqlite::Connection::open(db_path).map_err(|e| AgentOSError::KernelError {
            reason: format!(
                "UserInbox: failed to open DB at {}: {}",
                db_path.display(),
                e
            ),
        })?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             PRAGMA temp_store=MEMORY;",
        )
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: PRAGMA failed: {e}"),
        })?;

        // Apply migrations inline (sync path).
        for (idx, migration) in MIGRATIONS.iter().enumerate() {
            let current_version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("UserInbox: user_version query failed: {e}"),
                })?;
            if (current_version as usize) <= idx {
                conn.execute_batch(migration)
                    .map_err(|e| AgentOSError::KernelError {
                        reason: format!("UserInbox: migration {idx} failed: {e}"),
                    })?;
                let new_version = idx + 1;
                conn.execute_batch(&format!("PRAGMA user_version = {new_version}"))
                    .map_err(|e| AgentOSError::KernelError {
                        reason: format!("UserInbox: set user_version failed: {e}"),
                    })?;
            }
        }

        Ok(Self {
            store: StoreHandle::from_conn(conn),
            max_inbox_size,
        })
    }

    /// Persist a new `UserMessage` to the inbox.
    ///
    /// If the inbox would exceed `max_inbox_size` after this insert, the oldest
    /// `PURGE_BATCH` read messages are deleted first.
    pub async fn write(&self, msg: &UserMessage) -> Result<(), AgentOSError> {
        let max_size = self.max_inbox_size;
        let msg = msg.clone();

        self.store
            .exec_mut(move |conn| {
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

                let from_json = serde_json::to_string(&msg.from)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                let kind_json = serde_json::to_string(&msg.kind)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                let priority_json = serde_json::to_string(&msg.priority)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                let interaction_json = msg
                    .interaction
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                let delivery_json = serde_json::to_string(&msg.delivery_status)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                let response_json = msg
                    .response
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
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
                        priority_json,
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
                )?;
                Ok(())
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: write failed: {e}"),
            })
    }

    /// Update the delivery status for a specific channel on a message.
    pub async fn update_delivery_status(
        &self,
        id: &NotificationID,
        channel: DeliveryChannel,
        status: DeliveryStatus,
    ) -> Result<(), AgentOSError> {
        let id_str = id.to_string();
        let channel_key = channel.to_string();

        self.store
            .exec_mut(move |conn| {
                // Load current delivery_status JSON, merge the update, write back.
                let current: Option<String> = conn
                    .query_row(
                        "SELECT delivery_status FROM user_messages WHERE id = ?1",
                        params![id_str.clone()],
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
                )?;
                Ok(())
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: update_delivery_status failed: {e}"),
            })
    }

    /// Mark a notification as read by the user.
    pub async fn mark_read(&self, id: &NotificationID) -> Result<(), AgentOSError> {
        let id_str = id.to_string();
        self.store
            .exec_mut(move |conn| {
                conn.execute(
                    "UPDATE user_messages SET read = 1 WHERE id = ?1",
                    params![id_str],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: mark_read failed: {e}"),
            })
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
        let id_str = id.to_string();
        let resp_json = serde_json::to_string(response).map_err(|e| AgentOSError::KernelError {
            reason: format!("UserInbox: response serialisation failed: {e}"),
        })?;

        self.store
            .exec_mut(move |conn| {
                let changed = conn.execute(
                    "UPDATE user_messages SET response = ?1 WHERE id = ?2 AND response IS NULL",
                    params![resp_json, id_str.clone()],
                )?;
                if changed == 0 {
                    // Use QueryReturnedNoRows as a sentinel; the caller maps this to AgentOSError.
                    return Err(rusqlite::Error::QueryReturnedNoRows);
                }
                Ok(())
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!(
                    "UserInbox: notification {id} not found or already has a response: {e}"
                ),
            })
    }

    /// List messages from the inbox, ordered by creation time (newest first).
    pub async fn list(
        &self,
        unread_only: bool,
        limit: usize,
    ) -> Result<Vec<UserMessage>, AgentOSError> {
        self.store
            .exec(move |conn| {
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
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(params![limit as i64], row_to_user_message)?;
                let mut msgs = Vec::new();
                for row in rows {
                    match row {
                        Ok(Ok(msg)) => msgs.push(msg),
                        Ok(Err(e)) => tracing::warn!("UserInbox: skipping malformed row: {}", e),
                        Err(e) => return Err(e),
                    }
                }
                Ok(msgs)
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: list failed: {e}"),
            })
    }

    /// Fetch a single message by ID.
    pub async fn get(&self, id: &NotificationID) -> Result<Option<UserMessage>, AgentOSError> {
        let id_str = id.to_string();
        self.store
            .exec(move |conn| {
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
                    Ok(Err(_e)) => {
                        // Malformed row — treat as not found rather than hard error.
                        Ok(None)
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e),
                }
            })
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserInbox: get failed: {e}"),
            })
    }

    /// Count unread messages using a single `COUNT(*)` query — used by the web bell counter.
    pub async fn count_unread(&self) -> usize {
        match self
            .store
            .exec(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM user_messages WHERE read = 0",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            })
            .await
        {
            Ok(n) => n as usize,
            Err(e) => {
                tracing::warn!("UserInbox: count_unread failed: {}", e);
                0
            }
        }
    }

    /// Return all unanswered interactive messages (blocking questions with no response).
    ///
    /// Used by `InboundRouter` to auto-route a reply when exactly one task is waiting.
    pub async fn list_pending_questions(&self) -> Vec<UserMessage> {
        match self
            .store
            .exec(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, from_source, task_id, trace_id, kind, priority, subject, body,
                            interaction, delivery_status, response, created_at, expires_at, read,
                            thread_id, reply_to_external_id
                     FROM user_messages
                     WHERE response IS NULL AND interaction IS NOT NULL
                     ORDER BY created_at ASC",
                )?;
                let rows = stmt.query_map([], row_to_user_message)?;
                let mut msgs = Vec::new();
                for row in rows {
                    match row {
                        Ok(Ok(msg)) => msgs.push(msg),
                        Ok(Err(e)) => tracing::warn!(
                            "UserInbox: skipping malformed row in list_pending_questions: {}",
                            e
                        ),
                        Err(e) => tracing::warn!(
                            "UserInbox: row fetch error in list_pending_questions: {}",
                            e
                        ),
                    }
                }
                Ok(msgs)
            })
            .await
        {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::warn!("UserInbox: list_pending_questions failed: {}", e);
                vec![]
            }
        }
    }

    /// Return all un-responded Question messages whose `expires_at` is in the past.
    pub async fn list_expired_questions(&self, now: chrono::DateTime<Utc>) -> Vec<UserMessage> {
        let now_str = now.to_rfc3339();
        match self
            .store
            .exec(move |conn| {
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
                let mut msgs = Vec::new();
                for row in rows {
                    match row {
                        Ok(Ok(msg)) => msgs.push(msg),
                        Ok(Err(e)) => tracing::warn!(
                            "UserInbox: skipping malformed row in list_expired_questions: {}",
                            e
                        ),
                        Err(e) => tracing::warn!(
                            "UserInbox: row fetch error in list_expired_questions: {}",
                            e
                        ),
                    }
                }
                Ok(msgs)
            })
            .await
        {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::warn!("UserInbox: list_expired_questions failed: {}", e);
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
    let task_id = task_id_str.and_then(|s| {
        s.parse()
            .map_err(|e| tracing::warn!("UserInbox: failed to parse task_id '{}': {}", s, e))
            .ok()
    });
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
    let interaction = interaction_json.as_deref().and_then(|s| {
        serde_json::from_str(s)
            .map_err(|e| tracing::warn!("UserInbox: failed to deserialize interaction: {e}"))
            .ok()
    });
    let delivery_status = serde_json::from_str(&delivery_json).unwrap_or_default();
    let response = response_json.as_deref().and_then(|s| {
        serde_json::from_str(s)
            .map_err(|e| tracing::warn!("UserInbox: failed to deserialize response: {e}"))
            .ok()
    });

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
