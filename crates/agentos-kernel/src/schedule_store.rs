use agentos_types::delivery::DeliveryMode;
use agentos_types::schedule::{OnceJob, OnceJobState, TimerAction, TimerEntry};
use agentos_types::*;
use anyhow::{anyhow, Context};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const LATEST_MIGRATION_VERSION: i64 = 4;

/// Persistent storage for schedule primitives (`ScheduledJob`, `OnceJob`,
/// `TimerEntry`) and per-fire `ScheduledRun` records.
///
/// The manager holds the in-memory authoritative copy; this store is a
/// write-through backing so schedules survive kernel restarts and a
/// creator-filtered audit trail is available to Phase 4 visibility tools.
pub struct ScheduleStore {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl ScheduleStore {
    /// Open the schedule DB at `path`, creating the file and parent directory
    /// if needed and running pending migrations.
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for schedule DB: {}",
                        parent.display()
                    )
                })?;
            }
            let conn = Connection::open(&path_for_open).with_context(|| {
                format!("Failed to open schedule DB at {}", path_for_open.display())
            })?;
            Self::configure_connection(&conn)?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("Schedule DB open task failed")??;

        Ok(Self {
            path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // ── Schedules (cron) ──────────────────────────────────────────────────────

    pub async fn upsert_schedule(&self, job: ScheduledJob) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let permissions_json = serde_json::to_string(&job.permissions)
                .context("Serialize schedule permissions")?;
            let delivery_json =
                serde_json::to_string(&job.delivery).context("Serialize schedule delivery")?;
            let creator_id = job
                .creator_agent_id
                .map(|id| id.to_string())
                .unwrap_or_default();
            guard
                .execute(
                    "INSERT INTO schedules (
                        id, name, cron_expression, timezone, agent_name,
                        task_prompt, permissions_json, state, created_at,
                        last_run_at, next_run_at, run_count, max_retries, retry_count,
                        creator_agent_id, delivery_mode_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                    ON CONFLICT(id) DO UPDATE SET
                        name = excluded.name,
                        cron_expression = excluded.cron_expression,
                        timezone = excluded.timezone,
                        agent_name = excluded.agent_name,
                        task_prompt = excluded.task_prompt,
                        permissions_json = excluded.permissions_json,
                        state = excluded.state,
                        last_run_at = excluded.last_run_at,
                        next_run_at = excluded.next_run_at,
                        run_count = excluded.run_count,
                        max_retries = excluded.max_retries,
                        retry_count = excluded.retry_count,
                        creator_agent_id = excluded.creator_agent_id,
                        delivery_mode_json = excluded.delivery_mode_json",
                    params![
                        job.id.to_string(),
                        job.name,
                        job.cron_expression,
                        job.timezone,
                        job.agent_name,
                        job.task_prompt,
                        permissions_json,
                        schedule_state_str(&job.state),
                        job.created_at.to_rfc3339(),
                        job.last_run_at.map(|t| t.to_rfc3339()),
                        job.next_run_at.map(|t| t.to_rfc3339()),
                        job.run_count as i64,
                        i64::from(job.max_retries),
                        i64::from(job.retry_count),
                        creator_id,
                        delivery_json,
                    ],
                )
                .context("Failed to upsert schedule row")?;
            Ok(())
        })
        .await
        .context("Schedule upsert task failed")??;
        Ok(())
    }

    pub async fn delete_schedule(&self, id: ScheduleID) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            guard
                .execute(
                    "DELETE FROM schedules WHERE id = ?1",
                    params![id.to_string()],
                )
                .context("Failed to delete schedule row")?;
            Ok(())
        })
        .await
        .context("Schedule delete task failed")??;
        Ok(())
    }

    pub async fn load_all_schedules(&self) -> anyhow::Result<Vec<ScheduledJob>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ScheduledJob>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT id, name, cron_expression, timezone, agent_name,
                            task_prompt, permissions_json, state, created_at,
                            last_run_at, next_run_at, run_count, max_retries,
                            retry_count, creator_agent_id, delivery_mode_json
                     FROM schedules",
                )
                .context("Failed to prepare schedule list query")?;
            let rows = stmt
                .query_map([], Self::decode_schedule_row)
                .context("Failed to query schedule rows")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.context("Failed to decode schedule row")?);
            }
            Ok(out)
        })
        .await
        .context("Schedule load task failed")?
    }

    // ── Once jobs ─────────────────────────────────────────────────────────────

    pub async fn upsert_once_job(&self, job: OnceJob) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let delivery_json =
                serde_json::to_string(&job.delivery).context("Serialize once-job delivery")?;
            let creator_id = job
                .creator_agent_id
                .map(|id| id.to_string())
                .unwrap_or_default();
            guard
                .execute(
                    "INSERT INTO once_jobs (
                        id, name, agent_name, task_prompt, fire_at,
                        created_at, state, creator_agent_id, delivery_mode_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    ON CONFLICT(id) DO UPDATE SET
                        name = excluded.name,
                        agent_name = excluded.agent_name,
                        task_prompt = excluded.task_prompt,
                        fire_at = excluded.fire_at,
                        state = excluded.state,
                        creator_agent_id = excluded.creator_agent_id,
                        delivery_mode_json = excluded.delivery_mode_json",
                    params![
                        job.id.to_string(),
                        job.name,
                        job.agent_name,
                        job.task_prompt,
                        job.fire_at.to_rfc3339(),
                        job.created_at.to_rfc3339(),
                        once_state_str(&job.state),
                        creator_id,
                        delivery_json,
                    ],
                )
                .context("Failed to upsert once-job row")?;
            Ok(())
        })
        .await
        .context("Once-job upsert task failed")??;
        Ok(())
    }

    pub async fn delete_once_job(&self, id: ScheduleID) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            guard
                .execute(
                    "DELETE FROM once_jobs WHERE id = ?1",
                    params![id.to_string()],
                )
                .context("Failed to delete once-job row")?;
            Ok(())
        })
        .await
        .context("Once-job delete task failed")??;
        Ok(())
    }

    pub async fn load_all_once_jobs(&self) -> anyhow::Result<Vec<OnceJob>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<OnceJob>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT id, name, agent_name, task_prompt, fire_at,
                            created_at, state, creator_agent_id, delivery_mode_json
                     FROM once_jobs",
                )
                .context("Failed to prepare once-job list query")?;
            let rows = stmt
                .query_map([], Self::decode_once_job_row)
                .context("Failed to query once-job rows")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.context("Failed to decode once-job row")?);
            }
            Ok(out)
        })
        .await
        .context("Once-job load task failed")?
    }

    // ── Timers ────────────────────────────────────────────────────────────────

    pub async fn upsert_timer(&self, entry: TimerEntry) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let action_json =
                serde_json::to_string(&entry.action).context("Serialize timer action")?;
            let delivery_json =
                serde_json::to_string(&entry.delivery).context("Serialize timer delivery")?;
            let creator_id = entry
                .creator_agent_id
                .map(|id| id.to_string())
                .unwrap_or_default();
            guard
                .execute(
                    "INSERT INTO timers (
                        id, name, agent_name, fire_at, action_json, created_at,
                        creator_agent_id, delivery_mode_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    ON CONFLICT(id) DO UPDATE SET
                        name = excluded.name,
                        agent_name = excluded.agent_name,
                        fire_at = excluded.fire_at,
                        action_json = excluded.action_json,
                        creator_agent_id = excluded.creator_agent_id,
                        delivery_mode_json = excluded.delivery_mode_json",
                    params![
                        entry.id.to_string(),
                        entry.name,
                        entry.agent_name,
                        entry.fire_at.to_rfc3339(),
                        action_json,
                        entry.created_at.to_rfc3339(),
                        creator_id,
                        delivery_json,
                    ],
                )
                .context("Failed to upsert timer row")?;
            Ok(())
        })
        .await
        .context("Timer upsert task failed")??;
        Ok(())
    }

    pub async fn delete_timer(&self, id: ScheduleID) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            guard
                .execute("DELETE FROM timers WHERE id = ?1", params![id.to_string()])
                .context("Failed to delete timer row")?;
            Ok(())
        })
        .await
        .context("Timer delete task failed")??;
        Ok(())
    }

    pub async fn load_all_timers(&self) -> anyhow::Result<Vec<TimerEntry>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<TimerEntry>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT id, name, agent_name, fire_at, action_json, created_at,
                            creator_agent_id, delivery_mode_json
                     FROM timers",
                )
                .context("Failed to prepare timer list query")?;
            let rows = stmt
                .query_map([], Self::decode_timer_row)
                .context("Failed to query timer rows")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.context("Failed to decode timer row")?);
            }
            Ok(out)
        })
        .await
        .context("Timer load task failed")?
    }

    // ── Runs ──────────────────────────────────────────────────────────────────

    pub async fn upsert_run(&self, run: ScheduledRun) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let result_json = match &run.result {
                Some(v) => Some(serde_json::to_string(v).context("Serialize run result")?),
                None => None,
            };
            let tool_calls_json =
                serde_json::to_string(&run.tool_calls).context("Serialize tool_calls")?;
            let delivery_json =
                serde_json::to_string(&run.delivery).context("Serialize run delivery")?;
            guard
                .execute(
                    "INSERT INTO scheduled_runs (
                        run_id, parent_kind, parent_id, creator_agent_id, task_id,
                        state, started_at, completed_at, result_json, error,
                        tool_calls_json, delivered, delivered_at, delivery_error,
                        delivery_depth, delivery_mode_json, parent_name
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                    ON CONFLICT(run_id) DO UPDATE SET
                        state = excluded.state,
                        task_id = excluded.task_id,
                        completed_at = excluded.completed_at,
                        result_json = excluded.result_json,
                        error = excluded.error,
                        tool_calls_json = excluded.tool_calls_json,
                        delivered = excluded.delivered,
                        delivered_at = excluded.delivered_at,
                        delivery_error = excluded.delivery_error,
                        delivery_depth = excluded.delivery_depth,
                        delivery_mode_json = excluded.delivery_mode_json,
                        parent_name = excluded.parent_name",
                    params![
                        run.run_id.to_string(),
                        run.parent_kind.as_str(),
                        run.parent_id.to_string(),
                        run.creator_agent_id
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                        run.task_id.map(|id| id.to_string()),
                        run.state.as_str(),
                        run.started_at.to_rfc3339(),
                        run.completed_at.map(|t| t.to_rfc3339()),
                        result_json,
                        run.error,
                        tool_calls_json,
                        if run.delivered { 1_i64 } else { 0_i64 },
                        run.delivered_at.map(|t| t.to_rfc3339()),
                        run.delivery_error,
                        run.delivery_depth.map(i64::from),
                        delivery_json,
                        run.parent_name,
                    ],
                )
                .context("Failed to upsert scheduled_runs row")?;
            Ok(())
        })
        .await
        .context("Run upsert task failed")??;
        Ok(())
    }

    pub async fn get_run(&self, run_id: RunID) -> anyhow::Result<Option<ScheduledRun>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<ScheduledRun>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            guard
                .query_row(
                    "SELECT run_id, parent_kind, parent_id, creator_agent_id, task_id,
                            state, started_at, completed_at, result_json, error,
                            tool_calls_json, delivered, delivered_at, delivery_error,
                            delivery_depth, delivery_mode_json, parent_name
                     FROM scheduled_runs WHERE run_id = ?1",
                    params![run_id.to_string()],
                    Self::decode_run_row,
                )
                .optional()
                .context("Failed to query scheduled_runs row")
        })
        .await
        .context("Run read task failed")?
    }

    pub async fn list_runs_for_schedule(
        &self,
        parent_id: ScheduleID,
        limit: u32,
    ) -> anyhow::Result<Vec<ScheduledRun>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ScheduledRun>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT run_id, parent_kind, parent_id, creator_agent_id, task_id,
                            state, started_at, completed_at, result_json, error,
                            tool_calls_json, delivered, delivered_at, delivery_error,
                            delivery_depth, delivery_mode_json, parent_name
                     FROM scheduled_runs
                     WHERE parent_id = ?1
                     ORDER BY started_at DESC
                     LIMIT ?2",
                )
                .context("Failed to prepare run list query")?;
            let rows = stmt
                .query_map(
                    params![parent_id.to_string(), i64::from(limit)],
                    Self::decode_run_row,
                )
                .context("Failed to query run rows")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.context("Failed to decode run row")?);
            }
            Ok(out)
        })
        .await
        .context("Run list task failed")?
    }

    pub async fn list_runs_by_creator(
        &self,
        creator: AgentID,
        limit: u32,
    ) -> anyhow::Result<Vec<ScheduledRun>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ScheduledRun>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT run_id, parent_kind, parent_id, creator_agent_id, task_id,
                            state, started_at, completed_at, result_json, error,
                            tool_calls_json, delivered, delivered_at, delivery_error,
                            delivery_depth, delivery_mode_json, parent_name
                     FROM scheduled_runs
                     WHERE creator_agent_id = ?1
                     ORDER BY started_at DESC
                     LIMIT ?2",
                )
                .context("Failed to prepare run list query")?;
            let rows = stmt
                .query_map(
                    params![creator.to_string(), i64::from(limit)],
                    Self::decode_run_row,
                )
                .context("Failed to query run rows")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.context("Failed to decode run row")?);
            }
            Ok(out)
        })
        .await
        .context("Run list task failed")?
    }

    /// Delete `scheduled_runs` rows whose `completed_at` is older than
    /// `Utc::now() - max_age`. Rows that are still `Running`/`Queued` (no
    /// `completed_at`) are untouched so in-progress runs can never be
    /// evicted under the kernel's feet.
    pub async fn prune_runs_older_than(&self, max_age: chrono::Duration) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let cutoff = (chrono::Utc::now() - max_age).to_rfc3339();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Schedule DB mutex poisoned"))?;
            let deleted = guard
                .execute(
                    "DELETE FROM scheduled_runs
                     WHERE completed_at IS NOT NULL AND completed_at < ?1",
                    params![cutoff],
                )
                .context("Failed to prune expired runs")?;
            Ok(deleted)
        })
        .await
        .context("Run prune task failed")?
    }

    // ── Migrations / plumbing ─────────────────────────────────────────────────

    fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;",
        )
        .context("Failed to configure schedule DB pragmas")?;
        Ok(())
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schedule_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .context("Failed to create schedule meta table")?;

        let version: i64 = conn
            .query_row(
                "SELECT value FROM schedule_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("Failed to read schedule schema version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);

        if version > LATEST_MIGRATION_VERSION {
            anyhow::bail!(
                "Schedule DB schema version {} is newer than supported version {}",
                version,
                LATEST_MIGRATION_VERSION
            );
        }

        if version < 1 {
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS schedules (
                    id                 TEXT PRIMARY KEY,
                    name               TEXT NOT NULL UNIQUE,
                    cron_expression    TEXT NOT NULL,
                    timezone           TEXT,
                    agent_name         TEXT NOT NULL,
                    task_prompt        TEXT NOT NULL,
                    permissions_json   TEXT NOT NULL,
                    state              TEXT NOT NULL,
                    created_at         TEXT NOT NULL,
                    last_run_at        TEXT,
                    next_run_at        TEXT,
                    run_count          INTEGER NOT NULL DEFAULT 0,
                    max_retries        INTEGER NOT NULL DEFAULT 3,
                    retry_count        INTEGER NOT NULL DEFAULT 0,
                    output_destination TEXT  -- DEPRECATED v2: retained for upgrade compat only, never read/written after migration
                );
                CREATE INDEX IF NOT EXISTS idx_schedules_next_run
                    ON schedules(next_run_at);

                CREATE TABLE IF NOT EXISTS once_jobs (
                    id            TEXT PRIMARY KEY,
                    name          TEXT NOT NULL UNIQUE,
                    agent_name    TEXT NOT NULL,
                    task_prompt   TEXT NOT NULL,
                    fire_at       TEXT NOT NULL,
                    created_at    TEXT NOT NULL,
                    state         TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_once_fire_at
                    ON once_jobs(fire_at);

                CREATE TABLE IF NOT EXISTS timers (
                    id            TEXT PRIMARY KEY,
                    name          TEXT NOT NULL,
                    agent_name    TEXT NOT NULL,
                    fire_at       TEXT NOT NULL,
                    action_json   TEXT NOT NULL,
                    created_at    TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_timers_fire_at
                    ON timers(fire_at);

                CREATE TABLE IF NOT EXISTS scheduled_runs (
                    run_id            TEXT PRIMARY KEY,
                    parent_kind       TEXT NOT NULL,
                    parent_id         TEXT NOT NULL,
                    creator_agent_id  TEXT NOT NULL,
                    task_id           TEXT,
                    state             TEXT NOT NULL,
                    started_at        TEXT NOT NULL,
                    completed_at      TEXT,
                    result_json       TEXT,
                    error             TEXT,
                    tool_calls_json   TEXT NOT NULL,
                    delivered         INTEGER NOT NULL DEFAULT 0,
                    delivered_at      TEXT,
                    delivery_error    TEXT,
                    delivery_depth    INTEGER
                );
                CREATE INDEX IF NOT EXISTS idx_runs_parent
                    ON scheduled_runs(parent_id, started_at DESC);
                CREATE INDEX IF NOT EXISTS idx_runs_creator
                    ON scheduled_runs(creator_agent_id, started_at DESC);
                CREATE INDEX IF NOT EXISTS idx_runs_completed_at
                    ON scheduled_runs(completed_at);

                INSERT INTO schedule_meta(key, value)
                VALUES ('schema_version', '1')
                ON CONFLICT(key) DO UPDATE SET value = excluded.value;
                ",
            )
            .context("Failed to run schedule schema migration v1")?;
        }

        if version < 2 {
            // Add creator_agent_id and delivery_mode_json to the three schedule
            // tables so Phase 2 ownership and DeliveryMode tracking can be stored.
            // Existing rows get empty/silent defaults; they will be updated on the
            // next write-through when the manager modifies them.
            conn.execute_batch(
                "ALTER TABLE schedules ADD COLUMN creator_agent_id TEXT NOT NULL DEFAULT '';
                 ALTER TABLE schedules ADD COLUMN delivery_mode_json TEXT NOT NULL DEFAULT '{\"mode\":\"silent\"}';

                 ALTER TABLE once_jobs ADD COLUMN creator_agent_id TEXT NOT NULL DEFAULT '';
                 ALTER TABLE once_jobs ADD COLUMN delivery_mode_json TEXT NOT NULL DEFAULT '{\"mode\":\"silent\"}';

                 ALTER TABLE timers ADD COLUMN creator_agent_id TEXT NOT NULL DEFAULT '';
                 ALTER TABLE timers ADD COLUMN delivery_mode_json TEXT NOT NULL DEFAULT '{\"mode\":\"silent\"}';

                 INSERT INTO schedule_meta(key, value)
                 VALUES ('schema_version', '2')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
            )
            .context("Failed to run schedule schema migration v2")?;
        }

        if version < 3 {
            // Add delivery_mode_json to scheduled_runs so each run carries its
            // own delivery intent. Existing rows default to Silent so they are
            // never re-delivered spuriously after the migration.
            conn.execute_batch(
                "ALTER TABLE scheduled_runs ADD COLUMN delivery_mode_json TEXT NOT NULL DEFAULT '{\"mode\":\"silent\"}';

                 INSERT INTO schedule_meta(key, value)
                 VALUES ('schema_version', '3')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
            )
            .context("Failed to run schedule schema migration v3")?;
        }

        if version < 4 {
            // Add parent_name to scheduled_runs so delivery subjects are
            // human-readable even for Timers (which are evicted from memory
            // immediately after firing and have no name available at delivery time).
            conn.execute_batch(
                "ALTER TABLE scheduled_runs ADD COLUMN parent_name TEXT;

                 INSERT INTO schedule_meta(key, value)
                 VALUES ('schema_version', '4')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
            )
            .context("Failed to run schedule schema migration v4")?;
        }

        Ok(())
    }

    fn decode_schedule_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledJob> {
        // cols: 0=id 1=name 2=cron_expression 3=timezone 4=agent_name
        //       5=task_prompt 6=permissions_json 7=state 8=created_at
        //       9=last_run_at 10=next_run_at 11=run_count 12=max_retries
        //       13=retry_count 14=creator_agent_id 15=delivery_mode_json
        let id = parse_id::<ScheduleID>(row.get::<_, String>(0)?, "id").map_err(to_sql_error)?;
        let timezone: Option<String> = row.get(3)?;
        let permissions_json: String = row.get(6)?;
        let permissions: Vec<String> =
            serde_json::from_str(&permissions_json).map_err(|e| to_sql_error(anyhow!(e)))?;
        let state_str: String = row.get(7)?;
        let state = schedule_state_from_str(&state_str)
            .ok_or_else(|| to_sql_error(anyhow!("unknown ScheduleState variant: {}", state_str)))?;
        let creator_raw: String = row.get(14)?;
        let creator_agent_id = if creator_raw.is_empty() {
            None
        } else {
            Some(parse_id::<AgentID>(creator_raw, "creator_agent_id").map_err(to_sql_error)?)
        };
        let delivery_json: String = row.get(15)?;
        let delivery: DeliveryMode = if delivery_json.is_empty() {
            DeliveryMode::Silent
        } else {
            serde_json::from_str(&delivery_json).map_err(|e| to_sql_error(anyhow!(e)))?
        };
        Ok(ScheduledJob {
            id,
            name: row.get(1)?,
            cron_expression: row.get(2)?,
            timezone,
            creator_agent_id,
            agent_name: row.get(4)?,
            task_prompt: row.get(5)?,
            permissions,
            state,
            created_at: parse_ts(row.get::<_, String>(8)?, "created_at").map_err(to_sql_error)?,
            last_run_at: parse_ts_opt(row.get::<_, Option<String>>(9)?, "last_run_at")
                .map_err(to_sql_error)?,
            next_run_at: parse_ts_opt(row.get::<_, Option<String>>(10)?, "next_run_at")
                .map_err(to_sql_error)?,
            run_count: row.get::<_, i64>(11)? as u64,
            max_retries: row.get::<_, i64>(12)? as u32,
            retry_count: row.get::<_, i64>(13)? as u32,
            delivery,
        })
    }

    fn decode_once_job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OnceJob> {
        // cols: 0=id 1=name 2=agent_name 3=task_prompt 4=fire_at
        //       5=created_at 6=state 7=creator_agent_id 8=delivery_mode_json
        let id = parse_id::<ScheduleID>(row.get::<_, String>(0)?, "id").map_err(to_sql_error)?;
        let state_str: String = row.get(6)?;
        let state = once_state_from_str(&state_str)
            .ok_or_else(|| to_sql_error(anyhow!("unknown OnceJobState variant: {}", state_str)))?;
        let creator_raw: String = row.get(7)?;
        let creator_agent_id = if creator_raw.is_empty() {
            None
        } else {
            Some(parse_id::<AgentID>(creator_raw, "creator_agent_id").map_err(to_sql_error)?)
        };
        let delivery_json: String = row.get(8)?;
        let delivery: DeliveryMode = if delivery_json.is_empty() {
            DeliveryMode::Silent
        } else {
            serde_json::from_str(&delivery_json).map_err(|e| to_sql_error(anyhow!(e)))?
        };
        Ok(OnceJob {
            id,
            name: row.get(1)?,
            creator_agent_id,
            agent_name: row.get(2)?,
            task_prompt: row.get(3)?,
            fire_at: parse_ts(row.get::<_, String>(4)?, "fire_at").map_err(to_sql_error)?,
            created_at: parse_ts(row.get::<_, String>(5)?, "created_at").map_err(to_sql_error)?,
            state,
            delivery,
        })
    }

    fn decode_timer_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TimerEntry> {
        // cols: 0=id 1=name 2=agent_name 3=fire_at 4=action_json
        //       5=created_at 6=creator_agent_id 7=delivery_mode_json
        let id = parse_id::<ScheduleID>(row.get::<_, String>(0)?, "id").map_err(to_sql_error)?;
        let action_json: String = row.get(4)?;
        let action: TimerAction =
            serde_json::from_str(&action_json).map_err(|e| to_sql_error(anyhow!(e)))?;
        let creator_raw: String = row.get(6)?;
        let creator_agent_id = if creator_raw.is_empty() {
            None
        } else {
            Some(parse_id::<AgentID>(creator_raw, "creator_agent_id").map_err(to_sql_error)?)
        };
        let delivery_json: String = row.get(7)?;
        let delivery: DeliveryMode = if delivery_json.is_empty() {
            DeliveryMode::Silent
        } else {
            serde_json::from_str(&delivery_json).map_err(|e| to_sql_error(anyhow!(e)))?
        };
        Ok(TimerEntry {
            id,
            name: row.get(1)?,
            agent_name: row.get(2)?,
            fire_at: parse_ts(row.get::<_, String>(3)?, "fire_at").map_err(to_sql_error)?,
            action,
            created_at: parse_ts(row.get::<_, String>(5)?, "created_at").map_err(to_sql_error)?,
            creator_agent_id,
            delivery,
        })
    }

    fn decode_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledRun> {
        let run_id = parse_id::<RunID>(row.get::<_, String>(0)?, "run_id").map_err(to_sql_error)?;
        let kind_str: String = row.get(1)?;
        let parent_kind = RunParentKind::parse(&kind_str)
            .ok_or_else(|| to_sql_error(anyhow!("unknown RunParentKind variant: {}", kind_str)))?;
        let parent_id =
            parse_id::<ScheduleID>(row.get::<_, String>(2)?, "parent_id").map_err(to_sql_error)?;
        let creator_raw: String = row.get(3)?;
        let creator_agent_id = if creator_raw.is_empty() {
            None
        } else {
            Some(parse_id::<AgentID>(creator_raw, "creator_agent_id").map_err(to_sql_error)?)
        };
        let task_id = match row.get::<_, Option<String>>(4)? {
            Some(s) => Some(parse_id::<TaskID>(s, "task_id").map_err(to_sql_error)?),
            None => None,
        };
        let state_str: String = row.get(5)?;
        let state = RunState::parse(&state_str)
            .ok_or_else(|| to_sql_error(anyhow!("unknown RunState variant: {}", state_str)))?;
        let started_at = parse_ts(row.get::<_, String>(6)?, "started_at").map_err(to_sql_error)?;
        let completed_at =
            parse_ts_opt(row.get::<_, Option<String>>(7)?, "completed_at").map_err(to_sql_error)?;
        let result_json: Option<String> = row.get(8)?;
        let result = match result_json {
            Some(s) => Some(serde_json::from_str(&s).map_err(|e| to_sql_error(anyhow!(e)))?),
            None => None,
        };
        let error: Option<String> = row.get(9)?;
        let tool_calls_json: String = row.get(10)?;
        let tool_calls: Vec<ToolCallRecord> =
            serde_json::from_str(&tool_calls_json).map_err(|e| to_sql_error(anyhow!(e)))?;
        let delivered: i64 = row.get(11)?;
        let delivered_at = parse_ts_opt(row.get::<_, Option<String>>(12)?, "delivered_at")
            .map_err(to_sql_error)?;
        let delivery_error: Option<String> = row.get(13)?;
        // Reject out-of-range depths explicitly rather than silently truncating
        // to u8 — Phase 3 caps depth at 3, so any value >255 is corruption.
        let delivery_depth = match row.get::<_, Option<i64>>(14)? {
            Some(v) => Some(u8::try_from(v).map_err(|e| to_sql_error(anyhow!(e)))?),
            None => None,
        };
        // cols: 15=delivery_mode_json, 16=parent_name
        let delivery_json: String = row.get(15)?;
        let delivery: DeliveryMode = if delivery_json.is_empty() {
            DeliveryMode::Silent
        } else {
            serde_json::from_str(&delivery_json).map_err(|e| to_sql_error(anyhow!(e)))?
        };
        let parent_name: Option<String> = row.get(16)?;
        Ok(ScheduledRun {
            run_id,
            parent_kind,
            parent_id,
            parent_name,
            creator_agent_id,
            task_id,
            state,
            started_at,
            completed_at,
            result,
            error,
            tool_calls,
            delivery,
            delivered: delivered != 0,
            delivered_at,
            delivery_error,
            delivery_depth,
        })
    }
}

fn schedule_state_str(s: &ScheduleState) -> &'static str {
    match s {
        ScheduleState::Active => "Active",
        ScheduleState::Paused => "Paused",
        ScheduleState::Disabled => "Disabled",
    }
}

fn schedule_state_from_str(s: &str) -> Option<ScheduleState> {
    match s {
        "Active" => Some(ScheduleState::Active),
        "Paused" => Some(ScheduleState::Paused),
        "Disabled" => Some(ScheduleState::Disabled),
        _ => None,
    }
}

fn once_state_str(s: &OnceJobState) -> &'static str {
    match s {
        OnceJobState::Pending => "Pending",
        OnceJobState::Fired => "Fired",
        OnceJobState::Cancelled => "Cancelled",
    }
}

fn once_state_from_str(s: &str) -> Option<OnceJobState> {
    match s {
        "Pending" => Some(OnceJobState::Pending),
        "Fired" => Some(OnceJobState::Fired),
        "Cancelled" => Some(OnceJobState::Cancelled),
        _ => None,
    }
}

fn parse_ts(value: String, field: &'static str) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(&value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .with_context(|| format!("Invalid schedule {field} timestamp: {value}"))
}

fn parse_ts_opt(
    value: Option<String>,
    field: &'static str,
) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>> {
    match value {
        Some(v) => Ok(Some(parse_ts(v, field)?)),
        None => Ok(None),
    }
}

fn parse_id<T>(value: String, field: &'static str) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|err| anyhow!("Invalid schedule {field} '{}': {err}", value))
}

fn to_sql_error(err: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            err.to_string(),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::schedule::{OnceJob, OnceJobState, TimerAction, TimerEntry};

    async fn temp_store() -> (tempfile::TempDir, ScheduleStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ScheduleStore::open(dir.path().join("schedules.db"))
            .await
            .unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn schedule_round_trip() {
        let (_dir, store) = temp_store().await;
        let job = ScheduledJob {
            id: ScheduleID::new(),
            name: "nightly".into(),
            cron_expression: "0 0 0 * * *".into(),
            timezone: Some("America/New_York".into()),
            agent_name: "analyst".into(),
            task_prompt: "summarize".into(),
            permissions: vec!["read".into(), "write".into()],
            state: ScheduleState::Active,
            created_at: chrono::Utc::now(),
            last_run_at: None,
            next_run_at: Some(chrono::Utc::now()),
            run_count: 7,
            max_retries: 3,
            retry_count: 0,
            creator_agent_id: None,
            delivery: Default::default(),
        };
        store.upsert_schedule(job.clone()).await.unwrap();
        let all = store.load_all_schedules().await.unwrap();
        assert_eq!(all.len(), 1);
        let loaded = &all[0];
        assert_eq!(loaded.name, "nightly");
        assert_eq!(loaded.timezone.as_deref(), Some("America/New_York"));
        assert_eq!(loaded.permissions, vec!["read".to_string(), "write".into()]);
        assert_eq!(loaded.run_count, 7);

        store.delete_schedule(job.id).await.unwrap();
        assert!(store.load_all_schedules().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn once_job_round_trip() {
        let (_dir, store) = temp_store().await;
        let job = OnceJob {
            id: ScheduleID::new(),
            name: "one-shot".into(),
            creator_agent_id: None,
            agent_name: "runner".into(),
            task_prompt: "ping".into(),
            fire_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            created_at: chrono::Utc::now(),
            state: OnceJobState::Pending,
            delivery: Default::default(),
        };
        store.upsert_once_job(job.clone()).await.unwrap();
        let all = store.load_all_once_jobs().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, OnceJobState::Pending);
    }

    #[tokio::test]
    async fn timer_round_trip() {
        let (_dir, store) = temp_store().await;
        let entry = TimerEntry {
            id: ScheduleID::new(),
            name: "beep".into(),
            creator_agent_id: None,
            agent_name: "runner".into(),
            fire_at: chrono::Utc::now() + chrono::Duration::seconds(30),
            action: TimerAction::NotifyUser {
                subject: "hi".into(),
                body: "hello".into(),
                priority: "info".into(),
            },
            created_at: chrono::Utc::now(),
            delivery: Default::default(),
        };
        store.upsert_timer(entry.clone()).await.unwrap();
        let all = store.load_all_timers().await.unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].action {
            TimerAction::NotifyUser { subject, .. } => assert_eq!(subject, "hi"),
            _ => panic!("wrong action variant"),
        }
    }

    #[tokio::test]
    async fn run_round_trip_and_filters() {
        let (_dir, store) = temp_store().await;
        let creator = AgentID::new();
        let parent = ScheduleID::new();
        let run = ScheduledRun {
            run_id: RunID::new(),
            parent_kind: RunParentKind::Schedule,
            parent_id: parent,
            parent_name: None,
            creator_agent_id: Some(creator),
            task_id: Some(TaskID::new()),
            state: RunState::Running,
            started_at: chrono::Utc::now(),
            completed_at: None,
            result: None,
            error: None,
            tool_calls: Vec::new(),
            delivery: Default::default(),
            delivered: false,
            delivered_at: None,
            delivery_error: None,
            delivery_depth: None,
        };
        store.upsert_run(run.clone()).await.unwrap();

        // Update to Complete with a result and a tool call.
        let mut completed = run.clone();
        completed.state = RunState::Complete;
        completed.completed_at = Some(chrono::Utc::now());
        completed.result = Some(serde_json::json!({"ok": true}));
        completed.tool_calls = vec![ToolCallRecord {
            tool_name: "noop".into(),
            tool_call_id: None,
            input_json: "{}".into(),
            output_json: "{\"ok\":true}".into(),
            called_at: chrono::Utc::now(),
            duration_ms: 12,
            success: true,
        }];
        store.upsert_run(completed.clone()).await.unwrap();

        let loaded = store.get_run(run.run_id).await.unwrap().unwrap();
        assert_eq!(loaded.state, RunState::Complete);
        assert_eq!(loaded.tool_calls.len(), 1);
        assert_eq!(loaded.tool_calls[0].tool_name, "noop");
        assert_eq!(loaded.result, Some(serde_json::json!({"ok": true})));

        let by_parent = store.list_runs_for_schedule(parent, 10).await.unwrap();
        assert_eq!(by_parent.len(), 1);

        let by_creator = store.list_runs_by_creator(creator, 10).await.unwrap();
        assert_eq!(by_creator.len(), 1);

        // Creator filter excludes other creators
        let other = store
            .list_runs_by_creator(AgentID::new(), 10)
            .await
            .unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn prune_removes_only_old_completed_runs() {
        let (_dir, store) = temp_store().await;
        let creator = AgentID::new();
        let parent = ScheduleID::new();

        // Fresh completed run — should survive.
        let fresh = ScheduledRun {
            run_id: RunID::new(),
            parent_kind: RunParentKind::Schedule,
            parent_id: parent,
            parent_name: None,
            creator_agent_id: Some(creator),
            task_id: None,
            state: RunState::Complete,
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            result: None,
            error: None,
            tool_calls: Vec::new(),
            delivery: Default::default(),
            delivered: true,
            delivered_at: Some(chrono::Utc::now()),
            delivery_error: None,
            delivery_depth: None,
        };
        store.upsert_run(fresh).await.unwrap();

        // Ancient completed run — should be pruned.
        let ancient = ScheduledRun {
            run_id: RunID::new(),
            parent_kind: RunParentKind::Schedule,
            parent_id: parent,
            parent_name: None,
            creator_agent_id: Some(creator),
            task_id: None,
            state: RunState::Complete,
            started_at: chrono::Utc::now() - chrono::Duration::days(40),
            completed_at: Some(chrono::Utc::now() - chrono::Duration::days(40)),
            result: None,
            error: None,
            tool_calls: Vec::new(),
            delivery: Default::default(),
            delivered: true,
            delivered_at: None,
            delivery_error: None,
            delivery_depth: None,
        };
        store.upsert_run(ancient).await.unwrap();

        // Running run even if "old" — must NOT be pruned.
        let still_running = ScheduledRun {
            run_id: RunID::new(),
            parent_kind: RunParentKind::Schedule,
            parent_id: parent,
            parent_name: None,
            creator_agent_id: Some(creator),
            task_id: None,
            state: RunState::Running,
            started_at: chrono::Utc::now() - chrono::Duration::days(60),
            completed_at: None,
            result: None,
            error: None,
            tool_calls: Vec::new(),
            delivery: Default::default(),
            delivered: false,
            delivered_at: None,
            delivery_error: None,
            delivery_depth: None,
        };
        store.upsert_run(still_running).await.unwrap();

        let pruned = store
            .prune_runs_older_than(chrono::Duration::days(30))
            .await
            .unwrap();
        assert_eq!(pruned, 1);

        let remaining = store.list_runs_for_schedule(parent, 50).await.unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[tokio::test]
    async fn reopen_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("schedules.db");
        let store = ScheduleStore::open(db_path.clone()).await.unwrap();
        let job = ScheduledJob {
            id: ScheduleID::new(),
            name: "survive".into(),
            cron_expression: "0 0 * * * *".into(),
            timezone: None,
            agent_name: "runner".into(),
            task_prompt: "hello".into(),
            permissions: vec![],
            state: ScheduleState::Active,
            created_at: chrono::Utc::now(),
            last_run_at: None,
            next_run_at: None,
            run_count: 0,
            max_retries: 3,
            retry_count: 0,
            creator_agent_id: None,
            delivery: Default::default(),
        };
        store.upsert_schedule(job.clone()).await.unwrap();
        drop(store);

        let reopened = ScheduleStore::open(db_path).await.unwrap();
        let all = reopened.load_all_schedules().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "survive");
    }
}
