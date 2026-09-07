//! SQLite-backed persistence for the channel DM pairing allowlist.
//!
//! [`agentos_channels::pairing::PairingManager`] holds the allowlist in memory.
//! Without this store every kernel restart silently emptied it, which is worse
//! than it sounds: the allowlist gates escalation *delivery*
//! ([`crate::escalation_channel_sink::ChannelBroadcastSink`] skips when no
//! sender is paired) **and** inbound `/approve` handling, so a restart left
//! approval prompts undeliverable and unanswerable until someone noticed and
//! re-paired.
//!
//! The list is small and changes rarely, so the manager hands over the whole
//! snapshot on every mutation and this store rewrites the table in one
//! transaction. No diffing, no per-row bookkeeping.
//!
//! Note that persisting an allowlist persists *trust*: a paired sender stays
//! able to approve escalations until explicitly revoked with
//! `agentos channel pair revoke`. That is the intended behaviour — a restart is
//! not a revocation — but it does mean revocation is now the only way out.

use agentos_channels::pairing::{AllowedSender, PairingPersistence};
use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const LATEST_MIGRATION_VERSION: i64 = 1;

pub struct PairingStore {
    conn: Arc<Mutex<Connection>>,
}

impl PairingStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for pairing DB: {}",
                        parent.display()
                    )
                })?;
            }
            let conn = Connection::open(&path_for_open).with_context(|| {
                format!("Failed to open pairing DB at {}", path_for_open.display())
            })?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("Pairing DB open task failed")??;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS paired_senders (
                 channel_id  TEXT NOT NULL,
                 sender_id   TEXT NOT NULL,
                 approved_at TEXT NOT NULL,
                 label       TEXT,
                 PRIMARY KEY (channel_id, sender_id)
             );",
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_version (version) VALUES (?1)",
            params![LATEST_MIGRATION_VERSION],
        )?;
        Ok(())
    }

    /// Every paired sender, for seeding the manager at boot.
    pub async fn load_all(&self) -> anyhow::Result<Vec<AllowedSender>> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<AllowedSender>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("pairing DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare("SELECT channel_id, sender_id, approved_at, label FROM paired_senders")?;
            let rows = stmt.query_map([], |row| {
                let approved_at: String = row.get(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    approved_at,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (channel_id, sender_id, approved_at, label) = row?;
                // A row whose timestamp cannot be parsed still describes a real
                // approval; dropping the sender would silently revoke it, so
                // fall back to "now" rather than lose the grant.
                let approved_at = approved_at
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now());
                out.push(AllowedSender {
                    channel_id,
                    sender_id,
                    approved_at,
                    label,
                });
            }
            Ok(out)
        })
        .await
        .context("Pairing DB load task failed")?
    }
}

#[async_trait::async_trait]
impl PairingPersistence for PairingStore {
    async fn save(&self, senders: &[AllowedSender]) {
        let conn = Arc::clone(&self.conn);
        let senders = senders.to_vec();
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("pairing DB mutex poisoned"))?;
            let tx = guard.transaction()?;
            tx.execute("DELETE FROM paired_senders", [])?;
            for s in &senders {
                tx.execute(
                    "INSERT OR REPLACE INTO paired_senders
                         (channel_id, sender_id, approved_at, label)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        s.channel_id,
                        s.sender_id,
                        s.approved_at.to_rfc3339(),
                        s.label
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await;

        // Best-effort: a failed write must not break pairing for the live
        // session, but it does mean the grant will not survive a restart.
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(error = %e, "Failed to persist pairing allowlist — grants will not survive restart")
            }
            Err(e) => {
                tracing::error!(error = %e, "Pairing persistence task failed — grants will not survive restart")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the store: an approval survives a restart. Without
    /// this, the escalation sink finds no paired senders after every reboot and
    /// silently drops approval prompts.
    #[tokio::test]
    async fn allowlist_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pairing.db");

        let store = PairingStore::open(path.clone()).await.unwrap();
        assert!(store.load_all().await.unwrap().is_empty());
        store
            .save(&[AllowedSender {
                channel_id: "chan-1".to_string(),
                sender_id: "1130156019".to_string(),
                approved_at: Utc::now(),
                label: Some("telegram".to_string()),
            }])
            .await;

        let reopened = PairingStore::open(path).await.unwrap();
        let loaded = reopened.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].sender_id, "1130156019");
        assert_eq!(loaded[0].label.as_deref(), Some("telegram"));

        // A revoke clears the row rather than leaving a stale grant behind.
        reopened.save(&[]).await;
        assert!(reopened.load_all().await.unwrap().is_empty());
    }
}
