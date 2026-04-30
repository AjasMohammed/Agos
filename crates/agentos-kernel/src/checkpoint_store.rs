use crate::context::PersistedTaskContext;
use agentos_types::{AgentID, AgentTask, TaskID, ToolCallRecord};
use anyhow::{anyhow, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const CHECKPOINT_SCHEMA_VERSION: i64 = 1;
pub const CHECKPOINT_KEY_VERSION: i64 = 1;
const LATEST_MIGRATION_VERSION: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointPayload {
    pub schema_version: i64,
    pub key_version: i64,
    pub task: AgentTask,
    pub context: PersistedTaskContext,
    pub tool_call_history: Vec<ToolCallRecord>,
}

#[derive(Debug, Clone)]
pub struct CheckpointRecord {
    pub checkpoint_id: String,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub step_num: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub schema_version: i64,
    pub key_version: i64,
    pub state_blob: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CheckpointSummary {
    pub checkpoint_id: String,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub step_num: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub schema_version: i64,
    pub key_version: i64,
}

pub struct CheckpointStore {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl CheckpointStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let path_for_open = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path_for_open.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for checkpoint DB: {}",
                        parent.display()
                    )
                })?;
            }

            let conn = Connection::open(&path_for_open).with_context(|| {
                format!(
                    "Failed to open checkpoint DB at {}",
                    path_for_open.display()
                )
            })?;
            Self::configure_connection(&conn)?;
            Self::run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("Checkpoint DB open task failed")??;

        Ok(Self {
            path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn write(&self, record: CheckpointRecord) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Checkpoint DB mutex poisoned"))?;
            guard
                .execute(
                    "INSERT INTO checkpoints (
                        task_id, checkpoint_id, agent_id, step_num, created_at,
                        updated_at, schema_version, key_version, state_blob
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    ON CONFLICT(task_id) DO UPDATE SET
                        checkpoint_id = excluded.checkpoint_id,
                        agent_id = excluded.agent_id,
                        step_num = excluded.step_num,
                        created_at = excluded.created_at,
                        updated_at = excluded.updated_at,
                        schema_version = excluded.schema_version,
                        key_version = excluded.key_version,
                        state_blob = excluded.state_blob",
                    params![
                        record.task_id.to_string(),
                        record.checkpoint_id,
                        record.agent_id.to_string(),
                        i64::from(record.step_num),
                        record.created_at.to_rfc3339(),
                        record.updated_at.to_rfc3339(),
                        record.schema_version,
                        record.key_version,
                        record.state_blob,
                    ],
                )
                .context("Failed to upsert checkpoint row")?;
            Ok(())
        })
        .await
        .context("Checkpoint write task failed")??;
        Ok(())
    }

    pub async fn get_latest(&self, task_id: &TaskID) -> anyhow::Result<Option<CheckpointRecord>> {
        let conn = self.conn.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<CheckpointRecord>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Checkpoint DB mutex poisoned"))?;
            guard
                .query_row(
                    "SELECT checkpoint_id, task_id, agent_id, step_num, created_at,
                            updated_at, schema_version, key_version, state_blob
                     FROM checkpoints
                     WHERE task_id = ?1",
                    params![task_id],
                    Self::decode_record_row,
                )
                .optional()
                .context("Failed to query checkpoint row")
        })
        .await
        .context("Checkpoint read task failed")?
    }

    pub async fn list_checkpoints(&self) -> anyhow::Result<Vec<CheckpointSummary>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<CheckpointSummary>> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Checkpoint DB mutex poisoned"))?;
            let mut stmt = guard
                .prepare(
                    "SELECT checkpoint_id, task_id, agent_id, step_num, created_at,
                            updated_at, schema_version, key_version
                     FROM checkpoints
                     ORDER BY updated_at DESC",
                )
                .context("Failed to prepare checkpoint list query")?;

            let rows = stmt
                .query_map([], |row| {
                    Ok(CheckpointSummary {
                        checkpoint_id: row.get(0)?,
                        task_id: parse_id::<TaskID>(row.get::<_, String>(1)?, "task_id")
                            .map_err(to_sql_error)?,
                        agent_id: parse_id::<AgentID>(row.get::<_, String>(2)?, "agent_id")
                            .map_err(to_sql_error)?,
                        step_num: row.get::<_, i64>(3)? as u32,
                        created_at: parse_ts(row.get::<_, String>(4)?, "created_at")
                            .map_err(to_sql_error)?,
                        updated_at: parse_ts(row.get::<_, String>(5)?, "updated_at")
                            .map_err(to_sql_error)?,
                        schema_version: row.get(6)?,
                        key_version: row.get(7)?,
                    })
                })
                .context("Failed to query checkpoint rows")?;

            let mut checkpoints = Vec::new();
            for row in rows {
                checkpoints.push(row.context("Failed to decode checkpoint summary row")?);
            }
            Ok(checkpoints)
        })
        .await
        .context("Checkpoint list task failed")?
    }

    pub async fn prune_older_than(&self, max_age: chrono::Duration) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let cutoff = (chrono::Utc::now() - max_age).to_rfc3339();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Checkpoint DB mutex poisoned"))?;
            let deleted = guard
                .execute(
                    "DELETE FROM checkpoints WHERE updated_at < ?1",
                    params![cutoff],
                )
                .context("Failed to prune expired checkpoints")?;
            Ok(deleted)
        })
        .await
        .context("Checkpoint prune task failed")?
    }

    pub async fn delete_for_task(&self, task_id: &TaskID) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|_| anyhow!("Checkpoint DB mutex poisoned"))?;
            guard
                .execute(
                    "DELETE FROM checkpoints WHERE task_id = ?1",
                    params![task_id],
                )
                .context("Failed to delete checkpoint row")?;
            Ok(())
        })
        .await
        .context("Checkpoint delete task failed")??;
        Ok(())
    }

    fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;",
        )
        .context("Failed to configure checkpoint DB pragmas")?;
        Ok(())
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS checkpoint_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            ",
        )
        .context("Failed to create checkpoint meta table")?;

        let version: i64 = conn
            .query_row(
                "SELECT value FROM checkpoint_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("Failed to read checkpoint schema version")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);

        if version > LATEST_MIGRATION_VERSION {
            anyhow::bail!(
                "Checkpoint DB schema version {} is newer than supported version {}",
                version,
                LATEST_MIGRATION_VERSION
            );
        }

        if version < 1 {
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS checkpoints (
                    task_id TEXT PRIMARY KEY,
                    checkpoint_id TEXT NOT NULL,
                    agent_id TEXT NOT NULL,
                    step_num INTEGER NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    schema_version INTEGER NOT NULL,
                    key_version INTEGER NOT NULL,
                    state_blob BLOB NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_checkpoints_agent_updated
                    ON checkpoints(agent_id, updated_at DESC);
                CREATE INDEX IF NOT EXISTS idx_checkpoints_updated_at
                    ON checkpoints(updated_at DESC);
                INSERT INTO checkpoint_meta(key, value)
                VALUES ('schema_version', '1')
                ON CONFLICT(key) DO UPDATE SET value = excluded.value;
                ",
            )
            .context("Failed to run checkpoint schema migration v1")?;
        }

        Ok(())
    }

    fn decode_record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointRecord> {
        Ok(CheckpointRecord {
            checkpoint_id: row.get(0)?,
            task_id: parse_id::<TaskID>(row.get::<_, String>(1)?, "task_id")
                .map_err(to_sql_error)?,
            agent_id: parse_id::<AgentID>(row.get::<_, String>(2)?, "agent_id")
                .map_err(to_sql_error)?,
            step_num: row.get::<_, i64>(3)? as u32,
            created_at: parse_ts(row.get::<_, String>(4)?, "created_at").map_err(to_sql_error)?,
            updated_at: parse_ts(row.get::<_, String>(5)?, "updated_at").map_err(to_sql_error)?,
            schema_version: row.get(6)?,
            key_version: row.get(7)?,
            state_blob: row.get(8)?,
        })
    }
}

fn parse_ts(value: String, field: &'static str) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(&value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .with_context(|| format!("Invalid checkpoint {field} timestamp: {value}"))
}

fn parse_id<T>(value: String, field: &'static str) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|err| anyhow!("Invalid checkpoint {field} '{}': {err}", value))
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

    #[tokio::test]
    async fn test_checkpoint_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointStore::open(dir.path().join("checkpoints.db"))
            .await
            .unwrap();
        let task = AgentTask::default();
        let record = CheckpointRecord {
            checkpoint_id: "cp-test".to_string(),
            task_id: task.id,
            agent_id: task.agent_id,
            step_num: 2,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            key_version: CHECKPOINT_KEY_VERSION,
            state_blob: vec![1, 2, 3],
        };

        store.write(record.clone()).await.unwrap();
        let loaded = store.get_latest(&task.id).await.unwrap().unwrap();

        assert_eq!(loaded.checkpoint_id, record.checkpoint_id);
        assert_eq!(loaded.task_id, record.task_id);
        assert_eq!(loaded.agent_id, record.agent_id);
        assert_eq!(loaded.step_num, 2);
        assert_eq!(loaded.state_blob, vec![1, 2, 3]);
    }

    #[test]
    fn persisted_task_context_legacy_entry_content_field() {
        use crate::context::PersistedTaskContext;

        let json = r#"{
            "window": {
                "id": "550e8400-e29b-41d4-a716-446655440001",
                "entries": [
                    {
                        "role": "User",
                        "content": "checkpoint legacy blob",
                        "timestamp": "2020-06-01T12:00:00Z",
                        "metadata": null,
                        "importance": 0.5,
                        "pinned": false,
                        "reference_count": 0,
                        "partition": "Active",
                        "category": "history",
                        "is_summary": false
                    }
                ],
                "max_entries": 80,
                "overflow_strategy": "fifo_eviction",
                "needs_checkpoint": false
            },
            "agent_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            "injected_sub_agents": []
        }"#;

        let ctx: PersistedTaskContext =
            serde_json::from_str(json).expect("legacy checkpoint payload");
        assert_eq!(ctx.window.entries.len(), 1);
        assert_eq!(
            ctx.window.entries[0].parts,
            vec![agentos_types::ContentPart::Text {
                text: "checkpoint legacy blob".into(),
            }]
        );
    }
}
