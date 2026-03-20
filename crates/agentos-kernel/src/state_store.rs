use crate::escalation::PendingEscalation;
use agentos_types::{AgentID, AgentTask, TaskState};
use anyhow::{anyhow, Context};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const LATEST_MIGRATION_VERSION: i64 = 1;

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

        let migrations: &[(i64, &str)] = &[(
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
        )];

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
