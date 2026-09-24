use crate::escalation::PendingEscalation;
use agentos_types::{AgentID, AgentTask, EventSubscription, SubscriptionID, TaskID, TaskState};
use anyhow::{anyhow, Context};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const LATEST_MIGRATION_VERSION: i64 = 6;

/// Persisted usage counters for an agent.
#[derive(Debug, Clone)]
pub struct PersistedCostSnapshot {
    pub agent_id: AgentID,
    pub agent_name: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_cost_usd: f64,
    pub tool_calls: u64,
    pub period_start: chrono::DateTime<chrono::Utc>,
    pub version: u64,
}

/// Durable index row for a filesystem/context snapshot.
///
/// The JSON blob at `blob_path` remains the payload; this row is what makes a
/// `rollback_ref` in the audit log resolvable after a kernel restart.
#[derive(Debug, Clone)]
pub struct SnapshotRow {
    pub snap_id: String,
    pub task_id: TaskID,
    pub agent_id: String,
    pub action_type: String,
    pub taken_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub restored: bool,
    pub blob_path: PathBuf,
    pub size_bytes: u64,
}

/// SQLite-backed persistence layer for kernel runtime state.
///
/// This store is shared by scheduler, escalation manager, and cost tracker.
/// All public methods are async and execute blocking SQLite I/O through
/// `tokio::task::spawn_blocking`.
pub struct KernelStateStore {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl KernelStateStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for state DB: {}",
                        parent.display()
                    )
                })?;
            }

            let conn = Connection::open(&path_for_open).with_context(|| {
                format!(
                    "Failed to open kernel state DB at {}",
                    path_for_open.display()
                )
            })?;
            Self::configure_connection(&conn)?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("State DB open task failed")??;

        Ok(Self {
            path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn upsert_scheduler_task(&self, task: AgentTask) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let payload = serde_json::to_vec(&task)
                .context("Failed to serialize scheduler task payload for persistence")?;
            let task_id = task.id.to_string();
            let agent_id = task.agent_id.to_string();
            let state = task_state_to_db(task.state);
            let enqueued_at = task.created_at.to_rfc3339();
            let updated_at = chrono::Utc::now().to_rfc3339();

            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            guard
                .execute(
                    "INSERT INTO scheduler_tasks (
                        task_id, agent_id, state, priority, enqueued_at, payload, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    ON CONFLICT(task_id) DO UPDATE SET
                        agent_id = excluded.agent_id,
                        state = excluded.state,
                        priority = excluded.priority,
                        enqueued_at = excluded.enqueued_at,
                        payload = excluded.payload,
                        updated_at = excluded.updated_at",
                    params![
                        task_id,
                        agent_id,
                        state,
                        i64::from(task.priority),
                        enqueued_at,
                        payload,
                        updated_at
                    ],
                )
                .context("Failed to upsert scheduler task")?;
            Ok(())
        })
        .await
        .context("Scheduler persistence task failed")??;
        Ok(())
    }

    pub async fn load_non_terminal_scheduler_tasks(&self) -> anyhow::Result<Vec<AgentTask>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<AgentTask>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;

            let mut stmt = guard
                .prepare(
                    "SELECT task_id, payload
                     FROM scheduler_tasks
                     WHERE state NOT IN ('complete', 'failed', 'cancelled')
                     ORDER BY priority DESC, enqueued_at ASC",
                )
                .context("Failed to prepare scheduler restore query")?;

            let rows = stmt
                .query_map([], |row| {
                    let task_id: String = row.get(0)?;
                    let payload: Vec<u8> = row.get(1)?;
                    Ok((task_id, payload))
                })
                .context("Failed to query scheduler restore rows")?;

            let mut tasks = Vec::new();
            for row in rows {
                let (task_id, payload) = row.context("Failed to decode scheduler restore row")?;
                match serde_json::from_slice::<AgentTask>(&payload) {
                    Ok(task) => tasks.push(task),
                    Err(err) => {
                        tracing::warn!(
                            task_id = %task_id,
                            error = %err,
                            "Skipping corrupted scheduler task payload during restore"
                        );
                    }
                }
            }
            Ok(tasks)
        })
        .await
        .context("Scheduler restore task failed")?
    }

    /// Delete specific scheduler rows by task ID.
    ///
    /// Backs `TaskScheduler::purge_agent_tasks` — the recovery valve for a
    /// runaway backlog. Chunked so the bound-parameter list stays well under
    /// SQLite's limit; IDs are always bound, never interpolated.
    pub async fn delete_scheduler_tasks(&self, task_ids: &[TaskID]) -> anyhow::Result<usize> {
        if task_ids.is_empty() {
            return Ok(0);
        }
        let ids: Vec<String> = task_ids.iter().map(|id| id.to_string()).collect();
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let mut deleted = 0usize;
            for chunk in ids.chunks(500) {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!("DELETE FROM scheduler_tasks WHERE task_id IN ({placeholders})");
                let params = rusqlite::params_from_iter(chunk.iter());
                deleted = deleted.saturating_add(
                    guard
                        .execute(&sql, params)
                        .context("Failed to delete scheduler tasks")?,
                );
            }
            Ok(deleted)
        })
        .await
        .context("Scheduler purge task failed")?
    }

    /// Delete terminal scheduler rows older than `max_age`.
    ///
    /// Terminal rows are already excluded from boot restore, but nothing ever
    /// removed them: the 2026-07-26 incident left 99,963 `failed` rows behind
    /// and took the state DB to 1.6 GB. Wired into the existing 10-minute
    /// sweep alongside checkpoint/session/schedule pruning.
    pub async fn prune_terminal_scheduler_tasks(
        &self,
        max_age: chrono::Duration,
    ) -> anyhow::Result<usize> {
        let cutoff = (chrono::Utc::now() - max_age).to_rfc3339();
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let deleted = guard
                .execute(
                    "DELETE FROM scheduler_tasks
                     WHERE state IN ('complete', 'failed', 'cancelled')
                       AND updated_at < ?1",
                    params![cutoff],
                )
                .context("Failed to prune terminal scheduler tasks")?;
            Ok(deleted)
        })
        .await
        .context("Scheduler prune task failed")?
    }

    /// Record the failure reason for a persisted task (no-op if the row is
    /// missing — the task may have been pruned already).
    pub async fn set_scheduler_task_error(
        &self,
        task_id: &TaskID,
        error: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let task_id = task_id.to_string();
        let error = error.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            guard
                .execute(
                    "UPDATE scheduler_tasks SET last_error = ?2 WHERE task_id = ?1",
                    params![task_id, error],
                )
                .context("Failed to record scheduler task error")?;
            Ok(())
        })
        .await
        .context("Scheduler error-persist task failed")?
    }

    /// Persist the final answer of a completed task next to its row so the
    /// task detail page can show it after the executor (and a restart) is gone.
    pub async fn set_scheduler_task_result(
        &self,
        task_id: &TaskID,
        result: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let task_id = task_id.to_string();
        let result = result.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            guard
                .execute(
                    "UPDATE scheduler_tasks SET last_result = ?2 WHERE task_id = ?1",
                    params![task_id, result],
                )
                .context("Failed to record scheduler task result")?;
            Ok(())
        })
        .await
        .context("Scheduler result-persist task failed")?
    }

    /// `(updated_at, last_result)` for a task row. For a terminal task
    /// `updated_at` is the moment of its last state change — its completion time.
    pub async fn scheduler_task_outcome(
        &self,
        task_id: &TaskID,
    ) -> anyhow::Result<Option<(chrono::DateTime<chrono::Utc>, Option<String>)>> {
        let conn = self.conn.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<_>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let row: Option<(String, Option<String>)> = guard
                .query_row(
                    "SELECT updated_at, last_result FROM scheduler_tasks WHERE task_id = ?1",
                    params![task_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .context("Failed to read scheduler task outcome")?;
            Ok(row.and_then(|(at, result)| {
                chrono::DateTime::parse_from_rfc3339(&at)
                    .ok()
                    .map(|at| (at.with_timezone(&chrono::Utc), result))
            }))
        })
        .await
        .context("Scheduler outcome read task failed")?
    }

    /// Most recent finished tasks (complete/failed/cancelled), newest first,
    /// with their failure reason. Used to rebuild task history after a
    /// restart; nothing here is ever re-queued.
    pub async fn load_recent_terminal_scheduler_tasks(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<(AgentTask, Option<String>)>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(
            move || -> anyhow::Result<Vec<(AgentTask, Option<String>)>> {
                let guard = conn
                    .lock()
                    .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
                let mut stmt = guard
                    .prepare(
                        "SELECT task_id, payload,
                            CASE WHEN state = 'failed' THEN last_error END
                     FROM scheduler_tasks
                     WHERE state IN ('complete', 'failed', 'cancelled')
                     ORDER BY updated_at DESC
                     LIMIT ?1",
                    )
                    .context("Failed to prepare scheduler history query")?;
                let rows = stmt
                    .query_map(params![limit as i64], |row| {
                        let task_id: String = row.get(0)?;
                        let payload: Vec<u8> = row.get(1)?;
                        let last_error: Option<String> = row.get(2)?;
                        Ok((task_id, payload, last_error))
                    })
                    .context("Failed to query scheduler history rows")?;
                let mut tasks = Vec::new();
                for row in rows {
                    let (task_id, payload, last_error) =
                        row.context("Failed to decode scheduler history row")?;
                    match serde_json::from_slice::<AgentTask>(&payload) {
                        Ok(task) => tasks.push((task, last_error)),
                        Err(err) => tracing::warn!(
                            task_id = %task_id,
                            error = %err,
                            "Skipping corrupted scheduler task payload during history load"
                        ),
                    }
                }
                Ok(tasks)
            },
        )
        .await
        .context("Scheduler history load task failed")?
    }

    pub async fn upsert_escalation(&self, escalation: PendingEscalation) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let payload = serde_json::to_vec(&escalation)
                .context("Failed to serialize escalation payload for persistence")?;
            let escalation_id = escalation.id.to_string();
            let task_id = escalation.task_id.to_string();
            let agent_id = escalation.agent_id.to_string();
            let risk_level = escalation.urgency.clone();
            let description = escalation
                .decision_point
                .chars()
                .take(512)
                .collect::<String>();
            let created_at = escalation.created_at.to_rfc3339();
            let expires_at = escalation.expires_at.to_rfc3339();
            let resolved = if escalation.resolved { 1_i64 } else { 0_i64 };
            let resolution = escalation.resolution.clone();
            let resolved_at = escalation.resolved_at.map(|ts| ts.to_rfc3339());

            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            guard
                .execute(
                    "INSERT INTO pending_escalations (
                        escalation_id, task_id, agent_id, risk_level, description,
                        created_at, expires_at, resolved, payload, resolution, resolved_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                    ON CONFLICT(escalation_id) DO UPDATE SET
                        task_id = excluded.task_id,
                        agent_id = excluded.agent_id,
                        risk_level = excluded.risk_level,
                        description = excluded.description,
                        created_at = excluded.created_at,
                        expires_at = excluded.expires_at,
                        resolved = excluded.resolved,
                        payload = excluded.payload,
                        resolution = excluded.resolution,
                        resolved_at = excluded.resolved_at",
                    params![
                        escalation_id,
                        task_id,
                        agent_id,
                        risk_level,
                        description,
                        created_at,
                        expires_at,
                        resolved,
                        payload,
                        resolution,
                        resolved_at
                    ],
                )
                .context("Failed to upsert escalation row")?;
            Ok(())
        })
        .await
        .context("Escalation persistence task failed")??;
        Ok(())
    }

    pub async fn load_unresolved_escalations(&self) -> anyhow::Result<Vec<PendingEscalation>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<PendingEscalation>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT escalation_id, payload
                     FROM pending_escalations
                     WHERE resolved = 0
                     ORDER BY created_at ASC",
                )
                .context("Failed to prepare escalation restore query")?;
            let rows = stmt
                .query_map([], |row| {
                    let escalation_id: String = row.get(0)?;
                    let payload: Vec<u8> = row.get(1)?;
                    Ok((escalation_id, payload))
                })
                .context("Failed to query escalation restore rows")?;

            let mut escalations = Vec::new();
            for row in rows {
                let (escalation_id, payload) =
                    row.context("Failed to decode escalation restore row")?;
                match serde_json::from_slice::<PendingEscalation>(&payload) {
                    Ok(escalation) => escalations.push(escalation),
                    Err(err) => {
                        tracing::warn!(
                            escalation_id = %escalation_id,
                            error = %err,
                            "Skipping corrupted escalation payload during restore"
                        );
                    }
                }
            }
            Ok(escalations)
        })
        .await
        .context("Escalation restore task failed")?
    }

    pub async fn next_escalation_id(&self) -> anyhow::Result<u64> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;

            // Filter to purely numeric IDs before CAST to avoid SQLite returning 0
            // for non-numeric strings (e.g. from data corruption), which could
            // produce a collision with an existing row.
            let max_id: Option<i64> = guard
                .query_row(
                    "SELECT MAX(CAST(escalation_id AS INTEGER)) FROM pending_escalations \
                     WHERE escalation_id GLOB '[0-9]*'",
                    [],
                    |row| row.get(0),
                )
                .context("Failed to compute max escalation ID")?;

            let next = match max_id {
                Some(value) if value >= 0 => (value as u64).saturating_add(1),
                _ => 1,
            };
            Ok(next)
        })
        .await
        .context("Escalation ID query task failed")?
    }

    pub async fn upsert_cost_snapshot(
        &self,
        snapshot: PersistedCostSnapshot,
    ) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;

            let input_tokens = clamp_u64_to_i64(snapshot.input_tokens);
            let output_tokens = clamp_u64_to_i64(snapshot.output_tokens);
            let tool_calls = clamp_u64_to_i64(snapshot.tool_calls);
            let version = clamp_u64_to_i64(snapshot.version);

            guard
                .execute(
                    "INSERT INTO cost_snapshots (
                        agent_id, agent_name, input_tokens, output_tokens,
                        total_cost_usd, tool_calls, period_start, version
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    ON CONFLICT(agent_id) DO UPDATE SET
                        agent_name = excluded.agent_name,
                        input_tokens = excluded.input_tokens,
                        output_tokens = excluded.output_tokens,
                        total_cost_usd = excluded.total_cost_usd,
                        tool_calls = excluded.tool_calls,
                        period_start = excluded.period_start,
                        version = excluded.version
                    WHERE excluded.version >= cost_snapshots.version",
                    params![
                        snapshot.agent_id.to_string(),
                        snapshot.agent_name,
                        input_tokens,
                        output_tokens,
                        snapshot.total_cost_usd,
                        tool_calls,
                        snapshot.period_start.to_rfc3339(),
                        version
                    ],
                )
                .context("Failed to upsert cost snapshot")?;
            Ok(())
        })
        .await
        .context("Cost snapshot persistence task failed")??;
        Ok(())
    }

    pub async fn load_cost_snapshots(&self) -> anyhow::Result<Vec<PersistedCostSnapshot>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<PersistedCostSnapshot>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;

            let mut stmt = guard
                .prepare(
                    "SELECT
                        agent_id, agent_name, input_tokens, output_tokens,
                        total_cost_usd, tool_calls, period_start, version
                     FROM cost_snapshots",
                )
                .context("Failed to prepare cost snapshot restore query")?;
            let rows = stmt
                .query_map([], |row| {
                    let agent_id: String = row.get(0)?;
                    let agent_name: String = row.get(1)?;
                    let input_tokens: i64 = row.get(2)?;
                    let output_tokens: i64 = row.get(3)?;
                    let total_cost_usd: f64 = row.get(4)?;
                    let tool_calls: i64 = row.get(5)?;
                    let period_start: String = row.get(6)?;
                    let version: i64 = row.get(7)?;
                    Ok((
                        agent_id,
                        agent_name,
                        input_tokens,
                        output_tokens,
                        total_cost_usd,
                        tool_calls,
                        period_start,
                        version,
                    ))
                })
                .context("Failed to query cost snapshots")?;

            let mut snapshots = Vec::new();
            for row in rows {
                let (
                    agent_id_str,
                    agent_name,
                    input_tokens,
                    output_tokens,
                    total_cost_usd,
                    tool_calls,
                    period_start,
                    version,
                ) = row.context("Failed to decode cost snapshot row")?;

                let agent_id = match agent_id_str.parse::<AgentID>() {
                    Ok(id) => id,
                    Err(err) => {
                        tracing::warn!(
                            agent_id = %agent_id_str,
                            error = %err,
                            "Skipping cost snapshot with invalid agent ID"
                        );
                        continue;
                    }
                };

                let period_start = match chrono::DateTime::parse_from_rfc3339(&period_start) {
                    Ok(ts) => ts.with_timezone(&chrono::Utc),
                    Err(err) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %err,
                            "Skipping cost snapshot with invalid period_start timestamp"
                        );
                        continue;
                    }
                };

                snapshots.push(PersistedCostSnapshot {
                    agent_id,
                    agent_name,
                    input_tokens: clamp_i64_to_u64(input_tokens),
                    output_tokens: clamp_i64_to_u64(output_tokens),
                    total_cost_usd: if total_cost_usd.is_finite() {
                        total_cost_usd.max(0.0)
                    } else {
                        0.0
                    },
                    tool_calls: clamp_i64_to_u64(tool_calls),
                    period_start,
                    version: clamp_i64_to_u64(version),
                });
            }

            Ok(snapshots)
        })
        .await
        .context("Cost snapshot restore task failed")?
    }

    pub async fn load_cost_snapshot(
        &self,
        agent_id: AgentID,
    ) -> anyhow::Result<Option<PersistedCostSnapshot>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<PersistedCostSnapshot>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;

            let row = guard
                .query_row(
                    "SELECT
                        agent_id, agent_name, input_tokens, output_tokens,
                        total_cost_usd, tool_calls, period_start, version
                     FROM cost_snapshots
                     WHERE agent_id = ?1",
                    params![agent_id.to_string()],
                    |row| {
                        let agent_id: String = row.get(0)?;
                        let agent_name: String = row.get(1)?;
                        let input_tokens: i64 = row.get(2)?;
                        let output_tokens: i64 = row.get(3)?;
                        let total_cost_usd: f64 = row.get(4)?;
                        let tool_calls: i64 = row.get(5)?;
                        let period_start: String = row.get(6)?;
                        let version: i64 = row.get(7)?;
                        Ok((
                            agent_id,
                            agent_name,
                            input_tokens,
                            output_tokens,
                            total_cost_usd,
                            tool_calls,
                            period_start,
                            version,
                        ))
                    },
                )
                .optional()
                .context("Failed to query cost snapshot by agent_id")?;

            let Some((
                agent_id_str,
                agent_name,
                input_tokens,
                output_tokens,
                total_cost_usd,
                tool_calls,
                period_start,
                version,
            )) = row
            else {
                return Ok(None);
            };

            let parsed_agent_id = match agent_id_str.parse::<AgentID>() {
                Ok(id) => id,
                Err(err) => {
                    tracing::warn!(
                        agent_id = %agent_id_str,
                        error = %err,
                        "Ignoring cost snapshot row with invalid agent ID"
                    );
                    return Ok(None);
                }
            };

            let parsed_period_start = match chrono::DateTime::parse_from_rfc3339(&period_start) {
                Ok(ts) => ts.with_timezone(&chrono::Utc),
                Err(err) => {
                    tracing::warn!(
                        agent_id = %agent_id_str,
                        error = %err,
                        "Ignoring cost snapshot row with invalid period_start"
                    );
                    return Ok(None);
                }
            };

            Ok(Some(PersistedCostSnapshot {
                agent_id: parsed_agent_id,
                agent_name,
                input_tokens: clamp_i64_to_u64(input_tokens),
                output_tokens: clamp_i64_to_u64(output_tokens),
                total_cost_usd: if total_cost_usd.is_finite() {
                    total_cost_usd.max(0.0)
                } else {
                    0.0
                },
                tool_calls: clamp_i64_to_u64(tool_calls),
                period_start: parsed_period_start,
                version: clamp_i64_to_u64(version),
            }))
        })
        .await
        .context("Cost snapshot lookup task failed")?
    }

    // ── Event subscriptions ───────────────────────────────────────
    //
    // Durable half of `EventBus`, whose registry is otherwise an in-memory
    // `Vec` that a restart empties. Only long-lived subscriptions land here —
    // task-scoped and TTL ones are deliberately not persisted (see
    // `EventBus::subscribe_transient`).

    pub async fn upsert_event_subscription(&self, sub: EventSubscription) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let payload = serde_json::to_vec(&sub)
                .context("Failed to serialize event subscription for persistence")?;
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            guard
                .execute(
                    "INSERT INTO event_subscriptions (
                        subscription_id, agent_id, created_at, payload
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(subscription_id) DO UPDATE SET
                        agent_id = excluded.agent_id,
                        created_at = excluded.created_at,
                        payload = excluded.payload",
                    params![
                        sub.id.to_string(),
                        sub.agent_id.to_string(),
                        sub.created_at.to_rfc3339(),
                        payload
                    ],
                )
                .context("Failed to persist event subscription")?;
            Ok(())
        })
        .await
        .context("Event subscription persist task failed")?
    }

    pub async fn delete_event_subscription(&self, id: SubscriptionID) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            guard
                .execute(
                    "DELETE FROM event_subscriptions WHERE subscription_id = ?1",
                    params![id.to_string()],
                )
                .context("Failed to delete event subscription")?;
            Ok(())
        })
        .await
        .context("Event subscription delete task failed")?
    }

    pub async fn delete_event_subscriptions_for_agent(
        &self,
        agent_id: AgentID,
    ) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            guard
                .execute(
                    "DELETE FROM event_subscriptions WHERE agent_id = ?1",
                    params![agent_id.to_string()],
                )
                .context("Failed to delete event subscriptions for agent")?;
            Ok(())
        })
        .await
        .context("Event subscription agent-delete task failed")?
    }

    pub async fn load_event_subscriptions(&self) -> anyhow::Result<Vec<EventSubscription>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<EventSubscription>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT subscription_id, payload FROM event_subscriptions
                     ORDER BY created_at ASC",
                )
                .context("Failed to prepare event subscription query")?;
            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let payload: Vec<u8> = row.get(1)?;
                    Ok((id, payload))
                })
                .context("Failed to query event subscriptions")?;

            let mut out = Vec::new();
            for row in rows {
                let (id, payload) = row.context("Failed to read event subscription row")?;
                // A row whose payload no longer deserializes (an EventType
                // removed from the enum, say) is dropped rather than failing
                // the whole boot — the alternative is a kernel that will not
                // start until someone edits SQLite by hand.
                match serde_json::from_slice::<EventSubscription>(&payload) {
                    Ok(sub) => out.push(sub),
                    Err(err) => tracing::warn!(
                        subscription_id = %id,
                        error = %err,
                        "Dropping unreadable persisted event subscription"
                    ),
                }
            }
            Ok(out)
        })
        .await
        .context("Event subscription load task failed")?
    }

    fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            ",
        )
        .context("Failed to apply SQLite PRAGMA settings")?;
        Ok(())
    }

    // ── Notification routing matrix ─────────────────────────────────────────

    /// Every persisted `(event, channel, mode)` rule.
    pub async fn load_notification_routes(&self) -> anyhow::Result<Vec<(String, String, String)>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, String, String)>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare("SELECT event, channel, mode FROM notification_routes")
                .context("Failed to prepare notification route query")?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .context("Failed to query notification routes")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.context("Failed to read notification route row")?);
            }
            Ok(out)
        })
        .await
        .context("Notification route load task failed")?
    }

    /// Insert-or-update a batch of rules in one transaction.
    pub async fn upsert_notification_routes(
        &self,
        rows: Vec<(String, String, String)>,
    ) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let now = chrono::Utc::now().to_rfc3339();
            let mut guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let tx = guard
                .transaction()
                .context("Failed to begin notification route transaction")?;
            for (event, channel, mode) in &rows {
                tx.execute(
                    "INSERT INTO notification_routes (event, channel, mode, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(event, channel) DO UPDATE SET
                        mode = excluded.mode,
                        updated_at = excluded.updated_at",
                    params![event, channel, mode, now],
                )
                .context("Failed to upsert notification route")?;
            }
            tx.commit()
                .context("Failed to commit notification route transaction")?;
            Ok(())
        })
        .await
        .context("Notification route write task failed")?
    }

    /// Drop every rule for one channel — called when the channel is disconnected
    /// so the matrix does not accumulate rows for targets that no longer exist.
    pub async fn delete_notification_routes_for_channel(
        &self,
        channel: String,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let n = guard
                .execute(
                    "DELETE FROM notification_routes WHERE channel = ?1",
                    params![channel],
                )
                .context("Failed to delete notification routes for channel")?;
            Ok(n)
        })
        .await
        .context("Notification route delete task failed")?
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS kernel_state_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            ",
        )
        .context("Failed to create migration metadata table")?;

        let current_version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM kernel_state_migrations",
                [],
                |row| row.get(0),
            )
            .context("Failed to read state DB migration version")?;

        let migrations: &[(i64, &str)] = &[
            (
                1,
                "
            CREATE TABLE IF NOT EXISTS scheduler_tasks (
                task_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                state TEXT NOT NULL,
                priority INTEGER NOT NULL,
                enqueued_at TEXT NOT NULL,
                payload BLOB NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_scheduler_tasks_state
                ON scheduler_tasks(state);
            CREATE INDEX IF NOT EXISTS idx_scheduler_tasks_priority_created
                ON scheduler_tasks(priority DESC, enqueued_at ASC);

            CREATE TABLE IF NOT EXISTS pending_escalations (
                escalation_id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                risk_level TEXT NOT NULL,
                description TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                resolved INTEGER NOT NULL DEFAULT 0,
                payload BLOB NOT NULL,
                resolution TEXT,
                resolved_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_pending_escalations_resolved
                ON pending_escalations(resolved);
            CREATE INDEX IF NOT EXISTS idx_pending_escalations_expires
                ON pending_escalations(expires_at);

            CREATE TABLE IF NOT EXISTS cost_snapshots (
                agent_id TEXT PRIMARY KEY,
                agent_name TEXT NOT NULL DEFAULT '',
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                total_cost_usd REAL NOT NULL DEFAULT 0.0,
                tool_calls INTEGER NOT NULL DEFAULT 0,
                period_start TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 0
            );
            ",
            ),
            (
                2,
                "
            ALTER TABLE scheduler_tasks ADD COLUMN last_error TEXT;
            ",
            ),
            (
                3,
                "
            ALTER TABLE scheduler_tasks ADD COLUMN last_result TEXT;
            ",
            ),
            (
                4,
                "
            CREATE TABLE IF NOT EXISTS event_subscriptions (
                subscription_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                payload BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_event_subscriptions_agent
                ON event_subscriptions(agent_id);
            ",
            ),
            (
                5,
                "
            CREATE TABLE IF NOT EXISTS notification_routes (
                event      TEXT NOT NULL,
                channel    TEXT NOT NULL,
                mode       TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (event, channel)
            );
            ",
            ),
            (
                6,
                "
            CREATE TABLE IF NOT EXISTS snapshots (
                snap_id     TEXT PRIMARY KEY,
                task_id     TEXT NOT NULL,
                agent_id    TEXT NOT NULL,
                action_type TEXT NOT NULL,
                taken_at    TEXT NOT NULL,
                expires_at  TEXT NOT NULL,
                restored    INTEGER NOT NULL DEFAULT 0,
                blob_path   TEXT NOT NULL,
                size_bytes  INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_snapshots_task
                ON snapshots(task_id);
            CREATE INDEX IF NOT EXISTS idx_snapshots_expires
                ON snapshots(expires_at);
            ",
            ),
        ];

        for (version, ddl) in migrations {
            if *version <= current_version {
                continue;
            }

            // Wrap the DDL + version insert in a single transaction so a crash
            // between the two steps cannot leave the DB in a partially-migrated
            // state where the tables exist but the version is not recorded.
            conn.execute_batch("BEGIN")
                .context("Failed to begin migration transaction")?;
            let result = (|| -> anyhow::Result<()> {
                conn.execute_batch(ddl).with_context(|| {
                    format!("Failed to apply state DB migration version {}", version)
                })?;
                conn.execute(
                    "INSERT INTO kernel_state_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![*version, chrono::Utc::now().to_rfc3339()],
                )
                .with_context(|| {
                    format!(
                        "Failed to record successful state DB migration version {}",
                        version
                    )
                })?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    conn.execute_batch("COMMIT")
                        .context("Failed to commit migration transaction")?;
                    tracing::info!(version = *version, "Applied kernel state DB migration");
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
        }

        let final_version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM kernel_state_migrations",
                [],
                |row| row.get(0),
            )
            .context("Failed to read final migration version")?;

        if final_version < LATEST_MIGRATION_VERSION {
            return Err(anyhow!(
                "State DB migrations incomplete: expected at least {}, got {}",
                LATEST_MIGRATION_VERSION,
                final_version
            ));
        }

        Ok(())
    }

    // ---- Snapshots -------------------------------------------------------
    //
    // The durable index behind `AuditEntry.rollback_ref`. A snapshot id that
    // reaches the audit log must resolve from here, not from process memory,
    // or "reversible" stops being true across a restart.

    fn snapshot_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotRow> {
        let snap_id: String = row.get(0)?;
        let task_id_s: String = row.get(1)?;
        let taken_at_s: String = row.get(4)?;
        let expires_at_s: String = row.get(5)?;
        let blob_path_s: String = row.get(7)?;
        let size_bytes: i64 = row.get(8)?;
        Ok(SnapshotRow {
            snap_id,
            task_id: task_id_s.parse::<TaskID>().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            agent_id: row.get(2)?,
            action_type: row.get(3)?,
            taken_at: chrono::DateTime::parse_from_rfc3339(&taken_at_s)
                .map(|t| t.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            expires_at: chrono::DateTime::parse_from_rfc3339(&expires_at_s)
                .map(|t| t.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            restored: row.get::<_, i64>(6)? != 0,
            blob_path: PathBuf::from(blob_path_s),
            size_bytes: size_bytes.max(0) as u64,
        })
    }

    const SNAPSHOT_COLS: &'static str =
        "snap_id, task_id, agent_id, action_type, taken_at, expires_at, restored, blob_path, size_bytes";

    pub async fn insert_snapshot(&self, row: SnapshotRow) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            guard
                .execute(
                    "INSERT INTO snapshots (
                        snap_id, task_id, agent_id, action_type,
                        taken_at, expires_at, restored, blob_path, size_bytes
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    ON CONFLICT(snap_id) DO UPDATE SET
                        task_id = excluded.task_id,
                        agent_id = excluded.agent_id,
                        action_type = excluded.action_type,
                        taken_at = excluded.taken_at,
                        expires_at = excluded.expires_at,
                        restored = excluded.restored,
                        blob_path = excluded.blob_path,
                        size_bytes = excluded.size_bytes",
                    params![
                        row.snap_id,
                        row.task_id.to_string(),
                        row.agent_id,
                        row.action_type,
                        row.taken_at.to_rfc3339(),
                        row.expires_at.to_rfc3339(),
                        if row.restored { 1_i64 } else { 0_i64 },
                        row.blob_path.to_string_lossy().to_string(),
                        clamp_u64_to_i64(row.size_bytes),
                    ],
                )
                .context("Failed to insert snapshot row")?;
            Ok(())
        })
        .await
        .context("spawn_blocking insert_snapshot")?
    }

    pub async fn get_snapshot(&self, snap_id: &str) -> anyhow::Result<Option<SnapshotRow>> {
        let conn = self.conn.clone();
        let snap_id = snap_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<SnapshotRow>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let sql = format!(
                "SELECT {} FROM snapshots WHERE snap_id = ?1",
                Self::SNAPSHOT_COLS
            );
            guard
                .query_row(&sql, params![snap_id], Self::snapshot_row_from)
                .optional()
                .context("Failed to read snapshot row")
        })
        .await
        .context("spawn_blocking get_snapshot")?
    }

    pub async fn list_snapshots_for_task(
        &self,
        task_id: &TaskID,
    ) -> anyhow::Result<Vec<SnapshotRow>> {
        let conn = self.conn.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<SnapshotRow>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let sql = format!(
                "SELECT {} FROM snapshots WHERE task_id = ?1 ORDER BY taken_at ASC",
                Self::SNAPSHOT_COLS
            );
            let mut stmt = guard.prepare(&sql)?;
            let rows = stmt
                .query_map(params![task_id], Self::snapshot_row_from)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("Failed to read snapshots for task")?;
            Ok(rows)
        })
        .await
        .context("spawn_blocking list_snapshots_for_task")?
    }

    /// Marks a snapshot restored. Returns `true` if this call performed the
    /// transition (the row existed and was not already restored), so callers
    /// can use it as a single-winner guard.
    pub async fn mark_snapshot_restored(&self, snap_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.clone();
        let snap_id = snap_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let n = guard
                .execute(
                    "UPDATE snapshots SET restored = 1 WHERE snap_id = ?1 AND restored = 0",
                    params![snap_id],
                )
                .context("Failed to mark snapshot restored")?;
            Ok(n > 0)
        })
        .await
        .context("spawn_blocking mark_snapshot_restored")?
    }

    /// Rows whose `expires_at` is in the past, oldest first.
    pub async fn list_expired_snapshots(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<SnapshotRow>> {
        let conn = self.conn.clone();
        let cutoff = now.to_rfc3339();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<SnapshotRow>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let sql = format!(
                "SELECT {} FROM snapshots WHERE expires_at < ?1 ORDER BY expires_at ASC",
                Self::SNAPSHOT_COLS
            );
            let mut stmt = guard.prepare(&sql)?;
            let rows = stmt
                .query_map(params![cutoff], Self::snapshot_row_from)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("Failed to read expired snapshots")?;
            Ok(rows)
        })
        .await
        .context("spawn_blocking list_expired_snapshots")?
    }

    pub async fn delete_snapshot(&self, snap_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.clone();
        let snap_id = snap_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let n = guard
                .execute("DELETE FROM snapshots WHERE snap_id = ?1", params![snap_id])
                .context("Failed to delete snapshot row")?;
            Ok(n > 0)
        })
        .await
        .context("spawn_blocking delete_snapshot")?
    }

    /// Every indexed snapshot — used by boot reconciliation.
    pub async fn list_all_snapshots(&self) -> anyhow::Result<Vec<SnapshotRow>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<SnapshotRow>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Kernel state DB mutex poisoned"))?;
            let sql = format!("SELECT {} FROM snapshots", Self::SNAPSHOT_COLS);
            let mut stmt = guard.prepare(&sql)?;
            let rows = stmt
                .query_map([], Self::snapshot_row_from)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("Failed to list snapshots")?;
            Ok(rows)
        })
        .await
        .context("spawn_blocking list_all_snapshots")?
    }
}

fn task_state_to_db(state: TaskState) -> &'static str {
    match state {
        TaskState::Queued => "queued",
        TaskState::Running => "running",
        TaskState::Waiting => "waiting",
        TaskState::Suspended => "suspended",
        TaskState::Complete => "complete",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}

fn clamp_u64_to_i64(v: u64) -> i64 {
    if v > i64::MAX as u64 {
        i64::MAX
    } else {
        v as i64
    }
}

fn clamp_i64_to_u64(v: i64) -> u64 {
    if v <= 0 {
        0
    } else {
        v as u64
    }
}
