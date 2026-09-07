//! Durable work-item queue (`work.db`) with atomic checkout.
//!
//! This is the pull-based engine behind autonomous agent operation: long-lived
//! agents wake on the heartbeat tick, **atomically claim** one unblocked work
//! item, run it through the normal task path, then complete it (unblocking any
//! dependents). The atomic claim — `SELECT` a claimable row then
//! `UPDATE ... WHERE state='pending'` inside one transaction — guarantees no two
//! agents ever execute the same item ("no double-work"); a loser sees zero rows
//! updated and moves on. Items left `checked_out` by a crashed agent are
//! reclaimed once their lock expires.
//!
//! Modeled on [`crate::checkpoint_store::CheckpointStore`] /
//! [`crate::org_store::OrgStore`]: `Arc<Mutex<Connection>>`, async methods over
//! `spawn_blocking`, WAL, parameterized queries (no string interpolation).

use anyhow::{anyhow, Context};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LATEST_MIGRATION_VERSION: i64 = 1;

/// Lifecycle state of a work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkState {
    /// Claimable now.
    Pending,
    /// Waiting on a `blocked_on` dependency to complete.
    Blocked,
    /// Claimed by an agent (holds an execution lock until `lock_expires`).
    CheckedOut,
    /// Finished successfully.
    Done,
    /// Finished unsuccessfully.
    Failed,
}

impl WorkState {
    fn as_str(self) -> &'static str {
        match self {
            WorkState::Pending => "pending",
            WorkState::Blocked => "blocked",
            WorkState::CheckedOut => "checked_out",
            WorkState::Done => "done",
            WorkState::Failed => "failed",
        }
    }

    fn from_db(s: &str) -> WorkState {
        match s {
            "blocked" => WorkState::Blocked,
            "checked_out" => WorkState::CheckedOut,
            "done" => WorkState::Done,
            "failed" => WorkState::Failed,
            _ => WorkState::Pending,
        }
    }
}

/// A unit of work an agent can claim and execute.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub item_id: String,
    /// Agent name this item is assigned to. `None` = claimable by any agent.
    pub assignee_agent: Option<String>,
    pub title: String,
    pub prompt: String,
    /// Goal ancestry / "why" carried into the task prompt for context.
    pub goal_ancestry: Option<String>,
    pub state: WorkState,
    /// `item_id` this item waits on before becoming claimable.
    pub blocked_on: Option<String>,
    pub priority: i64,
}

/// Fields needed to create a new item; the store assigns the id and state.
#[derive(Debug, Clone, Default)]
pub struct NewWorkItem {
    pub assignee_agent: Option<String>,
    pub title: String,
    pub prompt: String,
    pub goal_ancestry: Option<String>,
    pub blocked_on: Option<String>,
    pub priority: i64,
}

pub struct WorkQueue {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl WorkQueue {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for work DB: {}",
                        parent.display()
                    )
                })?;
            }
            let conn = Connection::open(&path_for_open).with_context(|| {
                format!("Failed to open work DB at {}", path_for_open.display())
            })?;
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA synchronous=NORMAL;",
            )
            .context("Failed to configure work DB pragmas")?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("Work DB open task failed")??;

        Ok(Self {
            path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS work_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .context("Failed to create work meta table")?;

        let version: i64 = conn
            .query_row(
                "SELECT value FROM work_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("Failed to read work schema version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);

        if version > LATEST_MIGRATION_VERSION {
            anyhow::bail!(
                "Work DB schema version {} is newer than supported version {}",
                version,
                LATEST_MIGRATION_VERSION
            );
        }

        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS work_items (
                    item_id        TEXT PRIMARY KEY,
                    assignee_agent TEXT,
                    title          TEXT NOT NULL,
                    prompt         TEXT NOT NULL,
                    goal_ancestry  TEXT,
                    state          TEXT NOT NULL,
                    blocked_on     TEXT,
                    lock_owner     TEXT,
                    lock_expires   TEXT,
                    task_id        TEXT,
                    priority       INTEGER NOT NULL DEFAULT 5,
                    created_at     TEXT NOT NULL,
                    updated_at     TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_work_claimable
                    ON work_items(state, priority DESC, created_at);
                INSERT INTO work_meta(key, value) VALUES ('schema_version', '1')
                    ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
            )
            .context("Failed to run work schema migration v1")?;
        }
        Ok(())
    }

    /// Enqueue a new item. An item with a `blocked_on` dependency starts
    /// `Blocked` (not claimable) until that dependency completes; otherwise
    /// `Pending`. Returns the generated `item_id`.
    pub async fn create(&self, item: NewWorkItem) -> anyhow::Result<String> {
        let conn = self.conn.clone();
        let item_id = uuid::Uuid::new_v4().to_string();
        let id_ret = item_id.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;
            let state = if item.blocked_on.is_some() {
                WorkState::Blocked
            } else {
                WorkState::Pending
            };
            let now = chrono::Utc::now().to_rfc3339();
            guard
                .execute(
                    "INSERT INTO work_items (
                        item_id, assignee_agent, title, prompt, goal_ancestry,
                        state, blocked_on, priority, created_at, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                    params![
                        item_id,
                        item.assignee_agent,
                        item.title,
                        item.prompt,
                        item.goal_ancestry,
                        state.as_str(),
                        item.blocked_on,
                        item.priority,
                        now,
                    ],
                )
                .context("insert work item")?;
            Ok(())
        })
        .await
        .context("Work create task failed")??;
        Ok(id_ret)
    }

    /// Link a claimed item to the task executing it, so task completion can be
    /// matched back to the item. Called right after the heartbeat enqueues the
    /// task for a claimed item.
    pub async fn set_task(&self, item_id: &str, task_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let item_id = item_id.to_string();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;
            guard
                .execute(
                    "UPDATE work_items SET task_id = ?2, updated_at = ?3 WHERE item_id = ?1",
                    params![item_id, task_id, chrono::Utc::now().to_rfc3339()],
                )
                .context("link work item to task")?;
            Ok(())
        })
        .await
        .context("Work set_task task failed")??;
        Ok(())
    }

    /// Finish the checked-out item linked to `task_id`, if any. On `success` the
    /// item is marked `Done` and its dependents unblocked; otherwise `Failed`.
    /// Returns the ids of items that became claimable (empty when nothing matched
    /// or nothing unblocked). Idempotent: a task with no linked item is a no-op.
    pub async fn complete_by_task(
        &self,
        task_id: &str,
        success: bool,
    ) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;

            // Match the live (checked-out) item for this task. Restricting to
            // checked_out makes this idempotent across retries / double-fires.
            let item_id: Option<String> = guard
                .query_row(
                    "SELECT item_id FROM work_items WHERE task_id = ?1 AND state = 'checked_out'",
                    params![task_id],
                    |row| row.get(0),
                )
                .optional()
                .context("look up work item by task")?;
            let Some(item_id) = item_id else {
                return Ok(Vec::new());
            };

            let now = chrono::Utc::now().to_rfc3339();
            let final_state = if success {
                WorkState::Done
            } else {
                WorkState::Failed
            };
            guard
                .execute(
                    "UPDATE work_items SET state = ?2, lock_owner = NULL, lock_expires = NULL, updated_at = ?3 WHERE item_id = ?1",
                    params![item_id, final_state.as_str(), now],
                )
                .context("finish work item by task")?;

            if !success {
                return Ok(Vec::new());
            }

            let mut stmt = guard
                .prepare("SELECT item_id FROM work_items WHERE blocked_on = ?1 AND state = 'blocked'")
                .context("prepare dependents query")?;
            let unblocked: Vec<String> = stmt
                .query_map(params![item_id], |row| row.get::<_, String>(0))
                .context("query dependents")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect dependents")?;
            drop(stmt);
            guard
                .execute(
                    "UPDATE work_items SET state = 'pending', updated_at = ?2 WHERE blocked_on = ?1 AND state = 'blocked'",
                    params![item_id, now],
                )
                .context("unblock dependents")?;
            Ok(unblocked)
        })
        .await
        .context("Work complete_by_task failed")?
    }

    /// Atomically claim the highest-priority claimable item for `agent`, placing
    /// an execution lock valid for `lock_ttl`. Returns `None` when nothing is
    /// claimable. An item is claimable when it is `Pending` and either assigned
    /// to `agent` or unassigned. The `WHERE state='pending'` guard on the UPDATE
    /// makes the claim atomic: a racing claimer updates zero rows and gets `None`.
    pub async fn checkout(
        &self,
        agent: &str,
        lock_ttl: Duration,
    ) -> anyhow::Result<Option<WorkItem>> {
        let conn = self.conn.clone();
        let agent = agent.to_string();
        let ttl_secs = lock_ttl.as_secs().min(i64::MAX as u64) as i64;
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<WorkItem>> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;

            // Pick the best claimable candidate (highest priority, then oldest).
            // `state` is set optimistically to CheckedOut — the item is only
            // returned to the caller if the atomic claim below actually wins.
            let candidate: Option<WorkItem> = guard
                .query_row(
                    "SELECT item_id, assignee_agent, title, prompt, goal_ancestry, blocked_on, priority
                     FROM work_items
                     WHERE state = 'pending' AND (assignee_agent IS NULL OR assignee_agent = ?1)
                     ORDER BY priority DESC, created_at ASC
                     LIMIT 1",
                    params![agent],
                    |row| {
                        Ok(WorkItem {
                            item_id: row.get(0)?,
                            assignee_agent: row.get(1)?,
                            title: row.get(2)?,
                            prompt: row.get(3)?,
                            goal_ancestry: row.get(4)?,
                            blocked_on: row.get(5)?,
                            priority: row.get(6)?,
                            state: WorkState::CheckedOut,
                        })
                    },
                )
                .optional()
                .context("select claimable work item")?;

            let Some(item) = candidate else {
                return Ok(None);
            };

            let now = chrono::Utc::now();
            let lock_expires = (now + chrono::Duration::seconds(ttl_secs)).to_rfc3339();
            // Atomic claim — the state guard means a concurrent claimer loses here.
            let updated = guard
                .execute(
                    "UPDATE work_items
                     SET state = 'checked_out', lock_owner = ?2, lock_expires = ?3, updated_at = ?4
                     WHERE item_id = ?1 AND state = 'pending'",
                    params![item.item_id, agent, lock_expires, now.to_rfc3339()],
                )
                .context("claim work item")?;
            if updated == 0 {
                // Lost the race between SELECT and UPDATE.
                return Ok(None);
            }

            Ok(Some(item))
        })
        .await
        .context("Work checkout task failed")?
    }

    /// Mark an item `Done` and unblock any items waiting on it. Returns the ids
    /// of items that became claimable as a result.
    pub async fn complete(&self, item_id: &str) -> anyhow::Result<Vec<String>> {
        self.finish(item_id, WorkState::Done).await
    }

    /// Mark an item `Failed`. Dependents stay `Blocked` (a failed prerequisite
    /// should not silently release its dependents).
    pub async fn fail(&self, item_id: &str) -> anyhow::Result<()> {
        self.finish(item_id, WorkState::Failed).await.map(|_| ())
    }

    async fn finish(&self, item_id: &str, final_state: WorkState) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.clone();
        let item_id = item_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;
            let now = chrono::Utc::now().to_rfc3339();
            guard
                .execute(
                    "UPDATE work_items SET state = ?2, lock_owner = NULL, lock_expires = NULL, updated_at = ?3 WHERE item_id = ?1",
                    params![item_id, final_state.as_str(), now],
                )
                .context("finish work item")?;

            if final_state != WorkState::Done {
                return Ok(Vec::new());
            }

            // Collect dependents before flipping them so we can report what unblocked.
            let mut stmt = guard
                .prepare("SELECT item_id FROM work_items WHERE blocked_on = ?1 AND state = 'blocked'")
                .context("prepare dependents query")?;
            let unblocked: Vec<String> = stmt
                .query_map(params![item_id], |row| row.get::<_, String>(0))
                .context("query dependents")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect dependents")?;
            drop(stmt);

            guard
                .execute(
                    "UPDATE work_items SET state = 'pending', updated_at = ?2 WHERE blocked_on = ?1 AND state = 'blocked'",
                    params![item_id, now],
                )
                .context("unblock dependents")?;
            Ok(unblocked)
        })
        .await
        .context("Work finish task failed")?
    }

    /// Release a checked-out item back to `Pending` immediately (e.g. the
    /// claimer failed to enqueue a task for it), so it doesn't wait for the lock
    /// to expire. No-op if the item isn't checked out.
    pub async fn release(&self, item_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let item_id = item_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;
            guard
                .execute(
                    "UPDATE work_items
                     SET state = 'pending', lock_owner = NULL, lock_expires = NULL, updated_at = ?2
                     WHERE item_id = ?1 AND state = 'checked_out'",
                    params![item_id, chrono::Utc::now().to_rfc3339()],
                )
                .context("release work item")?;
            Ok(())
        })
        .await
        .context("Work release task failed")??;
        Ok(())
    }

    /// Reclaim items whose execution lock has expired (owner crashed): reset them
    /// to `Pending` so another agent can pick them up. Returns the count reclaimed.
    pub async fn reclaim_orphaned(&self) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;
            let now = chrono::Utc::now().to_rfc3339();
            let n = guard
                .execute(
                    "UPDATE work_items
                     SET state = 'pending', lock_owner = NULL, lock_expires = NULL, updated_at = ?1
                     WHERE state = 'checked_out' AND lock_expires IS NOT NULL AND lock_expires < ?1",
                    params![now],
                )
                .context("reclaim orphaned work items")?;
            Ok(n)
        })
        .await
        .context("Work reclaim task failed")?
    }

    /// Current lifecycle state of an item, or `None` if it doesn't exist. Used by
    /// diagnostics and end-to-end tests to observe the work-loop progressing.
    pub async fn state_of(&self, item_id: &str) -> anyhow::Result<Option<WorkState>> {
        let conn = self.conn.clone();
        let item_id = item_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<WorkState>> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;
            let state: Option<String> = guard
                .query_row(
                    "SELECT state FROM work_items WHERE item_id = ?1",
                    params![item_id],
                    |row| row.get(0),
                )
                .optional()
                .context("query item state")?;
            Ok(state.as_deref().map(WorkState::from_db))
        })
        .await
        .context("Work state_of task failed")?
    }

    /// Number of items currently claimable by `agent` (for diagnostics / wake gating).
    pub async fn claimable_count(&self, agent: &str) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let agent = agent.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn.lock().map_err(|_| anyhow!("Work DB mutex poisoned"))?;
            let n: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM work_items
                     WHERE state = 'pending' AND (assignee_agent IS NULL OR assignee_agent = ?1)",
                    params![agent],
                    |row| row.get(0),
                )
                .context("count claimable")?;
            Ok(n as usize)
        })
        .await
        .context("Work claimable_count task failed")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn queue() -> WorkQueue {
        let dir = tempfile::tempdir().unwrap();
        WorkQueue::open(dir.path().join("work.db")).await.unwrap()
    }

    fn item(title: &str) -> NewWorkItem {
        NewWorkItem {
            title: title.to_string(),
            prompt: format!("do {title}"),
            priority: 5,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn checkout_claims_then_empties() {
        let q = queue().await;
        q.create(item("a")).await.unwrap();
        let claimed = q.checkout("agent", Duration::from_secs(60)).await.unwrap();
        assert!(claimed.is_some());
        assert_eq!(claimed.unwrap().title, "a");
        // Nothing left to claim.
        assert!(q
            .checkout("agent", Duration::from_secs(60))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn no_double_work_under_concurrent_checkout() {
        let q = Arc::new(queue().await);
        q.create(item("only")).await.unwrap();

        // Fire many concurrent claims for the single item.
        let mut handles = Vec::new();
        for _ in 0..8 {
            let q = q.clone();
            handles.push(tokio::spawn(async move {
                q.checkout("agent", Duration::from_secs(60)).await.unwrap()
            }));
        }
        let mut winners = 0;
        for h in handles {
            if h.await.unwrap().is_some() {
                winners += 1;
            }
        }
        assert_eq!(winners, 1, "exactly one claimer may win the single item");
    }

    #[tokio::test]
    async fn blocked_item_is_not_claimable_until_dependency_done() {
        let q = queue().await;
        let a = q.create(item("a")).await.unwrap();
        let mut b = item("b");
        b.blocked_on = Some(a.clone());
        q.create(b).await.unwrap();

        // Only A is claimable; B is blocked.
        let first = q
            .checkout("agent", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.title, "a");
        assert!(q
            .checkout("agent", Duration::from_secs(60))
            .await
            .unwrap()
            .is_none());

        // Completing A unblocks B.
        let unblocked = q.complete(&a).await.unwrap();
        assert_eq!(unblocked.len(), 1);
        let second = q
            .checkout("agent", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.title, "b");
    }

    #[tokio::test]
    async fn assignee_scoping_respected() {
        let q = queue().await;
        let mut only_bob = item("bob-task");
        only_bob.assignee_agent = Some("bob".into());
        q.create(only_bob).await.unwrap();

        // Alice can't claim Bob's item.
        assert!(q
            .checkout("alice", Duration::from_secs(60))
            .await
            .unwrap()
            .is_none());
        // Bob can.
        assert!(q
            .checkout("bob", Duration::from_secs(60))
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn expired_lock_is_reclaimed() {
        let q = queue().await;
        q.create(item("a")).await.unwrap();
        // Claim with a zero-second TTL so the lock is already expired.
        let claimed = q.checkout("agent", Duration::from_secs(0)).await.unwrap();
        assert!(claimed.is_some());
        assert!(q
            .checkout("agent", Duration::from_secs(60))
            .await
            .unwrap()
            .is_none());

        let reclaimed = q.reclaim_orphaned().await.unwrap();
        assert_eq!(reclaimed, 1);
        // Now claimable again.
        assert!(q
            .checkout("agent2", Duration::from_secs(60))
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn complete_by_task_finishes_and_unblocks() {
        let q = queue().await;
        let a = q.create(item("a")).await.unwrap();
        let mut b = item("b");
        b.blocked_on = Some(a.clone());
        q.create(b).await.unwrap();

        // Claim A and link it to a task, then complete via the task id.
        let claimed = q
            .checkout("agent", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.item_id, a);
        q.set_task(&a, "task-123").await.unwrap();

        // A non-matching task id is a no-op.
        assert!(q
            .complete_by_task("task-999", true)
            .await
            .unwrap()
            .is_empty());

        // Completing the linked task marks A done and unblocks B.
        let unblocked = q.complete_by_task("task-123", true).await.unwrap();
        assert_eq!(unblocked.len(), 1);
        assert_eq!(
            q.checkout("agent", Duration::from_secs(60))
                .await
                .unwrap()
                .unwrap()
                .title,
            "b"
        );

        // Idempotent: completing again matches nothing.
        assert!(q
            .complete_by_task("task-123", true)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn complete_by_task_failure_does_not_unblock() {
        let q = queue().await;
        let a = q.create(item("a")).await.unwrap();
        let mut b = item("b");
        b.blocked_on = Some(a.clone());
        q.create(b).await.unwrap();
        q.checkout("agent", Duration::from_secs(60)).await.unwrap();
        q.set_task(&a, "t1").await.unwrap();

        // Failure marks A failed but B stays blocked (no unblock on failure).
        assert!(q.complete_by_task("t1", false).await.unwrap().is_empty());
        assert!(q
            .checkout("agent", Duration::from_secs(60))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn priority_orders_checkout() {
        let q = queue().await;
        let mut low = item("low");
        low.priority = 1;
        let mut high = item("high");
        high.priority = 9;
        q.create(low).await.unwrap();
        q.create(high).await.unwrap();
        let first = q
            .checkout("agent", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.title, "high", "higher priority claimed first");
    }
}
