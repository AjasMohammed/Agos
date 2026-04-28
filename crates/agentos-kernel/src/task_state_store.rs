use crate::intent_ledger::IntentLedger;
use agentos_types::{AgentOSError, TaskID};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Durable materialized view of the current structured task state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateSnapshot {
    pub task_id: TaskID,
    pub ledger: IntentLedger,
    pub last_event_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolling_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_cache_state: Option<serde_json::Value>,
    pub updated_at: DateTime<Utc>,
}

pub struct TaskStateStore {
    conn: Arc<Mutex<Connection>>,
}

impl TaskStateStore {
    /// Open (or create) the task state database.
    pub fn open(db_path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open task_state.db: {}", e))
        })?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(|e| AgentOSError::StorageError(format!("PRAGMA setup failed: {}", e)))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_state (
                task_id               TEXT PRIMARY KEY,
                ledger_json           TEXT NOT NULL,
                last_event_seq        INTEGER NOT NULL DEFAULT 0,
                rolling_summary       TEXT,
                prompt_cache_key      TEXT,
                retrieval_cache_json  TEXT,
                updated_at            TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_task_state_updated_at
                ON task_state(updated_at DESC);",
        )
        .map_err(|e| AgentOSError::StorageError(format!("Schema creation failed: {}", e)))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn upsert(&self, snapshot: &TaskStateSnapshot) -> Result<(), AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let task_id = snapshot.task_id.to_string();
        let ledger_json = serde_json::to_string(&snapshot.ledger)
            .map_err(|e| AgentOSError::StorageError(format!("Serialize ledger failed: {}", e)))?;
        let last_event_seq = snapshot.last_event_seq as i64;
        let rolling_summary = snapshot.rolling_summary.clone();
        let prompt_cache_key = snapshot.prompt_cache_key.clone();
        let retrieval_cache_json = snapshot
            .retrieval_cache_state
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| {
                AgentOSError::StorageError(format!("Serialize retrieval cache failed: {}", e))
            })?;
        let updated_at = snapshot.updated_at.to_rfc3339();

        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;
            guard
                .execute(
                    "INSERT INTO task_state
                        (task_id, ledger_json, last_event_seq, rolling_summary, prompt_cache_key, retrieval_cache_json, updated_at)
                     VALUES
                        (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(task_id) DO UPDATE SET
                        ledger_json = excluded.ledger_json,
                        last_event_seq = excluded.last_event_seq,
                        rolling_summary = excluded.rolling_summary,
                        prompt_cache_key = excluded.prompt_cache_key,
                        retrieval_cache_json = excluded.retrieval_cache_json,
                        updated_at = excluded.updated_at",
                    params![
                        task_id,
                        ledger_json,
                        last_event_seq,
                        rolling_summary,
                        prompt_cache_key,
                        retrieval_cache_json,
                        updated_at
                    ],
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))?
    }

    pub async fn read(&self, task_id: &TaskID) -> Result<Option<TaskStateSnapshot>, AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let task_id = task_id.to_string();

        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;
            let mut stmt = guard
                .prepare(
                    "SELECT task_id, ledger_json, last_event_seq, rolling_summary,
                            prompt_cache_key, retrieval_cache_json, updated_at
                     FROM task_state
                     WHERE task_id = ?1",
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            let snapshot = stmt
                .query_row(params![task_id], |row| {
                    let task_id_str: String = row.get(0)?;
                    let ledger_json: String = row.get(1)?;
                    let last_event_seq: i64 = row.get(2)?;
                    let rolling_summary: Option<String> = row.get(3)?;
                    let prompt_cache_key: Option<String> = row.get(4)?;
                    let retrieval_cache_json: Option<String> = row.get(5)?;
                    let updated_at: String = row.get(6)?;

                    let task_id = task_id_str.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                    let ledger = serde_json::from_str(&ledger_json).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                    let retrieval_cache_state = retrieval_cache_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;
                    let updated_at = updated_at.parse::<DateTime<Utc>>().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;

                    Ok(TaskStateSnapshot {
                        task_id,
                        ledger,
                        last_event_seq: last_event_seq.max(0) as u64,
                        rolling_summary,
                        prompt_cache_key,
                        retrieval_cache_state,
                        updated_at,
                    })
                })
                .optional()
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            Ok(snapshot)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))?
    }

    pub async fn delete(&self, task_id: &TaskID) -> Result<bool, AgentOSError> {
        let conn = Arc::clone(&self.conn);
        let task_id = task_id.to_string();

        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|e| AgentOSError::StorageError(format!("Lock poisoned: {}", e)))?;
            let affected = guard
                .execute(
                    "DELETE FROM task_state WHERE task_id = ?1",
                    params![task_id],
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            Ok(affected > 0)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Spawn blocking failed: {}", e)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::AgentID;
    use tempfile::tempdir;

    #[tokio::test]
    async fn round_trip_snapshot() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("task_state.db");
        let store = TaskStateStore::open(&db_path).unwrap();

        let task_id = TaskID::new();
        let agent_id = AgentID::new();
        let ledger = IntentLedger::from_task_prompt(task_id, agent_id, None, "Investigate issue");
        let snapshot = TaskStateSnapshot {
            task_id,
            ledger,
            last_event_seq: 42,
            rolling_summary: Some("Recent work summary".to_string()),
            prompt_cache_key: Some("cache-key".to_string()),
            retrieval_cache_state: Some(serde_json::json!({"semantic": ["fact-1"]})),
            updated_at: Utc::now(),
        };

        store.upsert(&snapshot).await.unwrap();
        let loaded = store.read(&task_id).await.unwrap().unwrap();

        assert_eq!(loaded.task_id, snapshot.task_id);
        assert_eq!(loaded.ledger.goal, "Investigate issue");
        assert_eq!(loaded.last_event_seq, 42);
        assert_eq!(
            loaded.rolling_summary.as_deref(),
            Some("Recent work summary")
        );
        assert_eq!(loaded.prompt_cache_key.as_deref(), Some("cache-key"));
    }
}
