//! SQLite-backed atomic task checkout.
//!
//! Gives dispatch a crash-safe, single-owner claim: exactly one agent can own a
//! task at a time, enforced by a `task_id PRIMARY KEY` row inserted with
//! `ON CONFLICT DO NOTHING`. Inspired by paperclip's atomic task checkout — once
//! multi-agent coordination lets several agents pull work, an in-memory claim is
//! not enough; a persisted row survives restarts and is reclaimable when its
//! lease expires.
//!
//! The claim is a lease: `expires_at` is set from the task's effective timeout
//! plus a margin so a normal-length run never loses its claim mid-flight, and the
//! `TimeoutChecker` sweeps expired rows so a crashed owner's task becomes
//! claimable again. All I/O runs on `spawn_blocking` (rusqlite is synchronous).

use agentos_types::{AgentID, AgentOSError, TaskID};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// `claimed_at` / `expires_at` are Unix epoch seconds (INTEGER) so the sweep's
// `expires_at < now` is a numeric comparison — no dependence on RFC3339 string
// layout (offset format, fractional-second precision) for correctness.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS task_checkout (
    task_id        TEXT PRIMARY KEY,
    owner_agent_id TEXT NOT NULL,
    claimed_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_checkout_expires ON task_checkout(expires_at);";

/// Atomic, crash-safe task ownership claims.
pub struct TaskCheckoutStore {
    conn: Arc<Mutex<Connection>>,
}

impl TaskCheckoutStore {
    /// Open (or create) the checkout database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open task_checkout.db: {e}"))
        })?;
        Self::init(conn)
    }

    /// In-memory store — fallback when the on-disk DB can't be opened, and for tests.
    pub fn in_memory() -> Result<Self, AgentOSError> {
        let conn = Connection::open_in_memory().map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open in-memory task_checkout: {e}"))
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

    /// Atomically claim ownership of a task for `owner` with a `lease`. Returns
    /// `true` only if THIS call inserted the row (became the owner); `false` means
    /// the task is already claimed (the expected race outcome, not an error).
    ///
    /// `ON CONFLICT DO NOTHING` makes the check-and-insert a single atomic
    /// statement, so two concurrent claimers cannot both succeed.
    pub async fn try_claim(
        &self,
        task_id: &TaskID,
        owner: &AgentID,
        lease: Duration,
    ) -> Result<bool, AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let tid = task_id.to_string();
        let oid = owner.to_string();
        let now = Utc::now();
        let claimed_at = now.timestamp();
        // Epoch seconds. Saturating conversion: a multi-hour lease never overflows
        // here, but guard against an absurd Duration rather than panicking.
        let expires_at = (now
            + chrono::Duration::from_std(lease).unwrap_or_else(|_| chrono::Duration::hours(24)))
        .timestamp();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("lock poisoned: {e}")))?;
            let inserted = guard
                .execute(
                    "INSERT INTO task_checkout (task_id, owner_agent_id, claimed_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(task_id) DO NOTHING",
                    params![tid, oid, claimed_at, expires_at],
                )
                .map_err(|e| AgentOSError::StorageError(format!("try_claim insert: {e}")))?;
            Ok(inserted == 1)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("spawn_blocking join: {e}")))?
    }

    /// Release a claim. Returns whether a row was removed (idempotent — releasing
    /// a never-claimed task is a harmless no-op returning `false`).
    pub async fn release(&self, task_id: &TaskID) -> Result<bool, AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let tid = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("lock poisoned: {e}")))?;
            let n = guard
                .execute("DELETE FROM task_checkout WHERE task_id = ?1", params![tid])
                .map_err(|e| AgentOSError::StorageError(format!("release delete: {e}")))?;
            Ok(n > 0)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("spawn_blocking join: {e}")))?
    }

    /// The current owner of a task, if claimed.
    pub async fn owner_of(&self, task_id: &TaskID) -> Result<Option<AgentID>, AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let tid = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("lock poisoned: {e}")))?;
            let owner = guard
                .query_row(
                    "SELECT owner_agent_id FROM task_checkout WHERE task_id = ?1",
                    params![tid],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| AgentOSError::StorageError(format!("owner_of query: {e}")))?;
            // Parse the stored UUID string back into an AgentID; a malformed row
            // (shouldn't happen) is treated as unowned rather than erroring.
            Ok(owner.and_then(|s| s.parse::<AgentID>().ok()))
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("spawn_blocking join: {e}")))?
    }

    /// Delete claims whose lease has expired. Returns the number reclaimed. A
    /// reclaimed task becomes claimable again on the next dispatch attempt.
    pub async fn sweep_expired(&self) -> Result<usize, AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let now = Utc::now().timestamp();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("lock poisoned: {e}")))?;
            // `<=`: a lease that has reached its expiry instant is expired. Only
            // affects the exact-boundary second; real future leases are unaffected.
            let n = guard
                .execute(
                    "DELETE FROM task_checkout WHERE expires_at <= ?1",
                    params![now],
                )
                .map_err(|e| AgentOSError::StorageError(format!("sweep_expired delete: {e}")))?;
            Ok(n)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("spawn_blocking join: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (TaskID, AgentID) {
        (TaskID::new(), AgentID::new())
    }

    #[tokio::test]
    async fn try_claim_first_wins() {
        let s = TaskCheckoutStore::in_memory().unwrap();
        let (task, a1) = ids();
        let a2 = AgentID::new();
        assert!(s
            .try_claim(&task, &a1, Duration::from_secs(60))
            .await
            .unwrap());
        // A second claim for the same task — by anyone — loses.
        assert!(!s
            .try_claim(&task, &a2, Duration::from_secs(60))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn release_allows_reclaim() {
        let s = TaskCheckoutStore::in_memory().unwrap();
        let (task, agent) = ids();
        assert!(s
            .try_claim(&task, &agent, Duration::from_secs(60))
            .await
            .unwrap());
        assert!(s.release(&task).await.unwrap());
        // Releasing again is a harmless no-op.
        assert!(!s.release(&task).await.unwrap());
        // After release the task is claimable again.
        assert!(s
            .try_claim(&task, &agent, Duration::from_secs(60))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn owner_of_reports_claimer() {
        let s = TaskCheckoutStore::in_memory().unwrap();
        let (task, agent) = ids();
        assert!(s.owner_of(&task).await.unwrap().is_none());
        s.try_claim(&task, &agent, Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(s.owner_of(&task).await.unwrap(), Some(agent));
    }

    #[tokio::test]
    async fn sweep_reclaims_expired() {
        let s = TaskCheckoutStore::in_memory().unwrap();
        let (task, agent) = ids();
        // Zero-length lease → already expired the instant it's written.
        s.try_claim(&task, &agent, Duration::from_secs(0))
            .await
            .unwrap();
        let reclaimed = s.sweep_expired().await.unwrap();
        assert_eq!(reclaimed, 1);
        // Reclaimed → claimable again.
        assert!(s
            .try_claim(&task, &agent, Duration::from_secs(60))
            .await
            .unwrap());
    }
}
