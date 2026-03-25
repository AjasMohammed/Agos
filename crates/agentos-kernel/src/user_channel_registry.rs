use agentos_types::{AgentOSError, ChannelInstanceID, ChannelKind, RegisteredChannel};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// SQLite-backed registry of user-connected bidirectional channels.
///
/// All async methods use `spawn_blocking` to keep SQLite I/O off the Tokio thread pool.
pub struct UserChannelRegistry {
    db: Arc<Mutex<Connection>>,
}

impl UserChannelRegistry {
    /// Open (or create) the channel registry at `db_path`.
    pub fn new(db_path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path).map_err(|e| AgentOSError::KernelError {
            reason: format!(
                "UserChannelRegistry: failed to open DB at {}: {}",
                db_path.display(),
                e
            ),
        })?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserChannelRegistry: PRAGMA failed: {e}"),
            })?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS user_channels (
                id              TEXT PRIMARY KEY,
                kind            TEXT NOT NULL,
                external_id     TEXT NOT NULL,
                display_name    TEXT NOT NULL,
                credential_key  TEXT NOT NULL DEFAULT '',
                reply_topic     TEXT,
                server_url      TEXT,
                connected_at    TEXT NOT NULL,
                last_active     TEXT NOT NULL,
                active          INTEGER NOT NULL DEFAULT 1
            );",
        )
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry: schema migration failed: {e}"),
        })?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
        })
    }

    /// Register a new channel (or update an existing one with the same ID).
    pub async fn register(&self, ch: RegisteredChannel) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let ch_id = ch.id.to_string();
        let kind = ch.kind.to_string();
        let ext = ch.external_id.clone();
        let disp = ch.display_name.clone();
        let cred = ch.credential_key.clone();
        let reply = ch.reply_topic.clone();
        let surl = ch.server_url.clone();
        let conn_at = ch.connected_at.to_rfc3339();
        let last = ch.last_active.to_rfc3339();

        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            conn.execute(
                "INSERT INTO user_channels
                    (id, kind, external_id, display_name, credential_key, reply_topic, server_url,
                     connected_at, last_active, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)
                 ON CONFLICT(id) DO UPDATE SET
                    kind=excluded.kind, external_id=excluded.external_id,
                    display_name=excluded.display_name, credential_key=excluded.credential_key,
                    reply_topic=excluded.reply_topic, server_url=excluded.server_url,
                    last_active=excluded.last_active, active=1",
                params![ch_id, kind, ext, disp, cred, reply, surl, conn_at, last],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserChannelRegistry::register failed: {e}"),
            })?;
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry::register task panicked: {e}"),
        })?
    }

    /// Mark a channel as inactive (soft-delete).
    pub async fn deregister(&self, id: &ChannelInstanceID) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let id_str = id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            conn.execute(
                "UPDATE user_channels SET active = 0 WHERE id = ?1",
                params![id_str],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserChannelRegistry::deregister failed: {e}"),
            })?;
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry::deregister task panicked: {e}"),
        })?
    }

    /// Return all active registered channels.
    pub async fn list_active(&self) -> Result<Vec<RegisteredChannel>, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, kind, external_id, display_name, credential_key, \
                     reply_topic, server_url, connected_at, last_active, active \
                     FROM user_channels WHERE active = 1 ORDER BY connected_at ASC",
                )
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("UserChannelRegistry::list_active prepare failed: {e}"),
                })?;

            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, bool>(9)?,
                    ))
                })
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("UserChannelRegistry::list_active query failed: {e}"),
                })?;

            let mut channels = Vec::new();
            for row in rows {
                let (id, kind_str, ext, disp, cred, reply, surl, conn_at, last, active) = row
                    .map_err(|e| AgentOSError::KernelError {
                        reason: format!("UserChannelRegistry::list_active row error: {e}"),
                    })?;
                channels.push(row_to_channel((
                    id, kind_str, ext, disp, cred, reply, surl, conn_at, last, active,
                ))?);
            }
            Ok(channels)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry::list_active task panicked: {e}"),
        })?
    }

    /// Fetch a single channel by its ID (active or inactive).
    pub async fn get_by_id(
        &self,
        id: &ChannelInstanceID,
    ) -> Result<Option<RegisteredChannel>, AgentOSError> {
        let db = self.db.clone();
        let id_str = id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let result = conn.query_row(
                "SELECT id, kind, external_id, display_name, credential_key, \
                 reply_topic, server_url, connected_at, last_active, active \
                 FROM user_channels WHERE id = ?1",
                params![id_str],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, bool>(9)?,
                    ))
                },
            );
            match result {
                Ok(row) => Ok(Some(row_to_channel(row)?)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(AgentOSError::KernelError {
                    reason: format!("UserChannelRegistry::get_by_id failed: {e}"),
                }),
            }
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry::get_by_id task panicked: {e}"),
        })?
    }

    /// Update the `last_active` timestamp of a registered channel.
    pub async fn update_last_active(&self, id: &ChannelInstanceID) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let id_str = id.to_string();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            conn.execute(
                "UPDATE user_channels SET last_active = ?1 WHERE id = ?2",
                params![now, id_str],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("UserChannelRegistry::update_last_active failed: {e}"),
            })?;
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry::update_last_active task panicked: {e}"),
        })?
    }
}

/// Convert a raw SQLite row to a `RegisteredChannel`.
///
/// The tuple order must match the SELECT column order used in `list_active` and `get_by_id`.
type ChannelRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    bool,
);

fn row_to_channel(
    (id, kind_str, ext, disp, cred, reply, surl, conn_at, last, active): ChannelRow,
) -> Result<RegisteredChannel, AgentOSError> {
    Ok(RegisteredChannel {
        id: id.parse().map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry: bad channel ID '{id}': {e}"),
        })?,
        kind: kind_str
            .parse()
            .unwrap_or_else(|_| ChannelKind::Custom(kind_str.clone())),
        external_id: ext,
        display_name: disp,
        credential_key: cred,
        reply_topic: reply,
        server_url: surl,
        connected_at: conn_at.parse().map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry: bad connected_at: {e}"),
        })?,
        last_active: last.parse().map_err(|e| AgentOSError::KernelError {
            reason: format!("UserChannelRegistry: bad last_active: {e}"),
        })?,
        active,
    })
}

// ── ChannelListenerRegistry ───────────────────────────────────────────────────

/// Manages the lifecycle of background listener tasks, one per active inbound channel.
///
/// When a channel is connected, `start()` is called to spawn a listener task that
/// forwards `InboundMessage`s via the shared `mpsc::Sender`.  When a channel is
/// disconnected, `stop()` aborts the task.
pub struct ChannelListenerRegistry {
    listeners: RwLock<HashMap<ChannelInstanceID, tokio::task::JoinHandle<()>>>,
}

impl ChannelListenerRegistry {
    pub fn new() -> Self {
        Self {
            listeners: RwLock::new(HashMap::new()),
        }
    }

    /// Spawn a listener for `adapter` if it supports inbound messages.
    ///
    /// The `JoinHandle` returned by `adapter.start_listening()` is stored directly,
    /// so `stop()` aborts the actual poll/SSE task rather than an outer wrapper.
    /// Messages are forwarded via `tx`.
    pub async fn start(
        &self,
        id: ChannelInstanceID,
        adapter: Arc<dyn crate::notification_router::DeliveryAdapter>,
        tx: tokio::sync::mpsc::Sender<crate::notification_router::InboundMessage>,
    ) {
        if !adapter.supports_inbound() {
            return;
        }
        match adapter.start_listening(tx).await {
            Ok(inner_handle) => {
                // Abort any existing listener for this channel (handles reconnect without disconnect).
                if let Some(old) = self.listeners.write().await.insert(id, inner_handle) {
                    old.abort();
                }
            }
            Err(e) => {
                tracing::warn!(
                    channel_id = %id,
                    error = %e,
                    "Channel listener failed to start"
                );
            }
        }
    }

    /// Abort the listener task for a channel.
    pub async fn stop(&self, id: &ChannelInstanceID) {
        if let Some(handle) = self.listeners.write().await.remove(id) {
            handle.abort();
        }
    }

    /// Abort all listener tasks (called on kernel shutdown).
    pub async fn stop_all(&self) {
        let mut map = self.listeners.write().await;
        for (_, handle) in map.drain() {
            handle.abort();
        }
    }
}

impl Default for ChannelListenerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn make_channel(kind: ChannelKind, ext: &str) -> RegisteredChannel {
        let now = Utc::now();
        RegisteredChannel {
            id: ChannelInstanceID::new(),
            kind,
            external_id: ext.to_string(),
            display_name: ext.to_string(),
            credential_key: String::new(),
            reply_topic: None,
            server_url: None,
            connected_at: now,
            last_active: now,
            active: true,
        }
    }

    #[tokio::test]
    async fn test_register_and_list() {
        let tmp = NamedTempFile::new().unwrap();
        let reg = UserChannelRegistry::new(tmp.path()).unwrap();
        let ch = make_channel(ChannelKind::Telegram, "123456789");
        let id = ch.id;
        reg.register(ch).await.unwrap();
        let list = reg.list_active().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].external_id, "123456789");
        assert!(matches!(list[0].kind, ChannelKind::Telegram));
        // deregister
        reg.deregister(&id).await.unwrap();
        let list2 = reg.list_active().await.unwrap();
        assert!(list2.is_empty());
    }

    #[tokio::test]
    async fn test_persists_across_reopen() {
        let tmp = NamedTempFile::new().unwrap();
        let reg = UserChannelRegistry::new(tmp.path()).unwrap();
        let ch = make_channel(ChannelKind::Ntfy, "my-topic");
        reg.register(ch).await.unwrap();
        drop(reg);

        let reg2 = UserChannelRegistry::new(tmp.path()).unwrap();
        let list = reg2.list_active().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].external_id, "my-topic");
    }

    #[tokio::test]
    async fn test_update_last_active() {
        let tmp = NamedTempFile::new().unwrap();
        let reg = UserChannelRegistry::new(tmp.path()).unwrap();
        let ch = make_channel(ChannelKind::Telegram, "99");
        let id = ch.id;
        reg.register(ch).await.unwrap();
        reg.update_last_active(&id).await.unwrap();
        let list = reg.list_active().await.unwrap();
        assert_eq!(list.len(), 1);
    }
}
