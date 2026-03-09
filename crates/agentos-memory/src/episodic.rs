use crate::types::{EpisodeType, EpisodicEntry};
use agentos_types::{AgentID, AgentOSError, PermissionOp, TaskID, TraceID};
use chrono::{DateTime, Utc};
use rusqlite::{params, params_from_iter, types::Value, Connection, Result as SqliteResult};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

pub struct EpisodicStore {
    db: Mutex<Connection>,
}

impl EpisodicStore {
    /// Open or create the episodic memory database.
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError> {
        let db_path = data_dir.join("episodic_memory.db");
        let conn = Connection::open(&db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open episodic memory DB: {}", e))
        })?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS episodic_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                entry_type TEXT NOT NULL,
                content TEXT NOT NULL,
                summary TEXT,
                metadata TEXT,
                timestamp TEXT NOT NULL,
                trace_id TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_episodes_task ON episodic_events(task_id);
            CREATE INDEX IF NOT EXISTS idx_episodes_agent ON episodic_events(agent_id);
            CREATE INDEX IF NOT EXISTS idx_episodes_type ON episodic_events(entry_type);
            CREATE INDEX IF NOT EXISTS idx_episodes_timestamp ON episodic_events(timestamp);

            CREATE VIRTUAL TABLE IF NOT EXISTS episodic_fts USING fts5(
                summary,
                content,
                content='episodic_events',
                content_rowid='id'
            );

            CREATE TRIGGER IF NOT EXISTS episodic_ai AFTER INSERT ON episodic_events BEGIN
              INSERT INTO episodic_fts(rowid, summary, content) VALUES (new.id, new.summary, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS episodic_ad AFTER DELETE ON episodic_events BEGIN
              INSERT INTO episodic_fts(episodic_fts, rowid, summary, content) VALUES('delete', old.id, old.summary, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS episodic_au AFTER UPDATE ON episodic_events BEGIN
              INSERT INTO episodic_fts(episodic_fts, rowid, summary, content) VALUES('delete', old.id, old.summary, old.content);
              INSERT INTO episodic_fts(rowid, summary, content) VALUES (new.id, new.summary, new.content);
            END;
        ",
        )
        .map_err(|e| {
            AgentOSError::StorageError(format!("Failed to init episodic memory tables: {}", e))
        })?;

        Ok(Self {
            db: Mutex::new(conn),
        })
    }

    /// Record a new episodic entry safely.
    pub fn record(
        &self,
        task_id: &TaskID,
        agent_id: &AgentID,
        entry_type: EpisodeType,
        content: &str,
        summary: Option<&str>,
        metadata: Option<serde_json::Value>,
        trace_id: &TraceID,
    ) -> Result<(), AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock episodic db for writing".to_string())
        })?;
        let timestamp = Utc::now().to_rfc3339();
        let metadata_str = metadata.map(|v| v.to_string());

        conn.execute(
            "INSERT INTO episodic_events (task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                task_id.as_uuid().to_string(),
                agent_id.as_uuid().to_string(),
                entry_type.as_str(),
                content,
                summary,
                metadata_str,
                timestamp,
                trace_id.as_uuid().to_string()
            ],
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to record episode: {}", e)))?;

        Ok(())
    }

    /// Record an already-materialized episodic entry object.
    pub fn record_entry(&self, entry: EpisodicEntry) -> Result<(), AgentOSError> {
        let EpisodicEntry {
            task_id,
            agent_id,
            entry_type,
            content,
            summary,
            metadata,
            trace_id,
            ..
        } = entry;
        self.record(
            &task_id,
            &agent_id,
            entry_type,
            &content,
            summary.as_deref(),
            metadata,
            &trace_id,
        )
    }

    /// Retrieve raw timeline of a task chronologically.
    pub fn timeline_by_task(
        &self,
        task_id: &TaskID,
        limit: u32,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock episodic db for reading".to_string())
        })?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id
                 FROM episodic_events WHERE task_id = ?1 ORDER BY timestamp DESC LIMIT ?2",
            )
            .map_err(|e| AgentOSError::StorageError(format!("Failed to prepare query: {}", e)))?;

        let task_id_str = task_id.as_uuid().to_string();
        let episode_iter = stmt
            .query_map(params![task_id_str, limit], Self::row_to_episode)
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to query task episodes: {}", e))
            })?;

        let mut episodes = Vec::new();
        for row in episode_iter {
            episodes.push(row.map_err(|e| {
                AgentOSError::StorageError(format!("Failed to parse episode row: {}", e))
            })?);
        }
        episodes.reverse();
        Ok(episodes)
    }

    /// Get complete history for a task in chronological order.
    pub fn task_history(&self, task_id: &TaskID) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock episodic db for reading".to_string())
        })?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id
                 FROM episodic_events WHERE task_id = ?1 ORDER BY timestamp ASC",
            )
            .map_err(|e| AgentOSError::StorageError(format!("Failed to prepare query: {}", e)))?;

        let episode_iter = stmt
            .query_map(params![task_id.as_uuid().to_string()], Self::row_to_episode)
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to query task history: {}", e))
            })?;

        let mut episodes = Vec::new();
        for row in episode_iter {
            episodes.push(row.map_err(|e| {
                AgentOSError::StorageError(format!("Failed to parse episode row: {}", e))
            })?);
        }
        Ok(episodes)
    }

    /// Full-text search within a task's event history.
    /// Caller must own the task (same agent_id observed in task events).
    pub fn recall_task(
        &self,
        task_id: &TaskID,
        caller_agent_id: &AgentID,
        query: &str,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        if !self.task_owned_by(task_id, caller_agent_id)? {
            return Err(AgentOSError::PermissionDenied {
                resource: format!("memory.episodic.task:{}", task_id),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        self.recall_task_with_permission(task_id, query, limit)
    }

    /// Full-text search within a task's history when caller permission has already been validated.
    pub fn recall_task_with_permission(
        &self,
        task_id: &TaskID,
        query: &str,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        self.search_events(query, Some(task_id), None, Self::to_u32_limit(limit))
    }

    /// Search across all tasks. Permission checks are expected at the caller boundary.
    pub fn recall_global(
        &self,
        query: &str,
        agent_id: Option<&AgentID>,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock episodic db for search".to_string())
        })?;

        let sanitized_query = format!("\"{}\"", query.replace('"', "\"\""));
        let mut sql = String::from(
            "SELECT e.id, e.task_id, e.agent_id, e.entry_type, e.content, e.summary, e.metadata, e.timestamp, e.trace_id
             FROM episodic_fts f
             JOIN episodic_events e ON f.rowid = e.id
             WHERE episodic_fts MATCH ?1",
        );
        let mut bindings: Vec<Value> = vec![Value::from(sanitized_query)];

        if let Some(a) = agent_id {
            sql.push_str(&format!(" AND e.agent_id = ?{}", bindings.len() + 1));
            bindings.push(Value::from(a.as_uuid().to_string()));
        }

        if let Some(ts) = since {
            sql.push_str(&format!(" AND e.timestamp >= ?{}", bindings.len() + 1));
            bindings.push(Value::from(ts.to_rfc3339()));
        }

        sql.push_str(&format!(
            " ORDER BY e.timestamp DESC LIMIT ?{}",
            bindings.len() + 1
        ));
        bindings.push(Value::from(Self::to_i64_limit(limit)));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| AgentOSError::StorageError(format!("FTS Prepare error: {}", e)))?;

        let episode_iter = stmt
            .query_map(params_from_iter(bindings.iter()), Self::row_to_episode)
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to search global episodes: {}", e))
            })?;

        let mut episodes = Vec::new();
        for row in episode_iter {
            episodes.push(row.map_err(|e| {
                AgentOSError::StorageError(format!("Failed to parse search result row: {}", e))
            })?);
        }

        Ok(episodes)
    }

    /// Recall past events via FTS5 search.
    pub fn search_events(
        &self,
        query: &str,
        filter_task: Option<&TaskID>,
        filter_agent: Option<&AgentID>,
        limit: u32,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock episodic db for search".to_string())
        })?;

        let sanitized_query = format!("\"{}\"", query.replace('"', "\"\""));
        let mut sql = String::from(
            "SELECT e.id, e.task_id, e.agent_id, e.entry_type, e.content, e.summary, e.metadata, e.timestamp, e.trace_id
             FROM episodic_fts f
             JOIN episodic_events e ON f.rowid = e.id
             WHERE episodic_fts MATCH ?1",
        );
        let mut bindings: Vec<Value> = vec![Value::from(sanitized_query)];

        if let Some(t) = filter_task {
            sql.push_str(&format!(" AND e.task_id = ?{}", bindings.len() + 1));
            bindings.push(Value::from(t.as_uuid().to_string()));
        }

        if let Some(a) = filter_agent {
            sql.push_str(&format!(" AND e.agent_id = ?{}", bindings.len() + 1));
            bindings.push(Value::from(a.as_uuid().to_string()));
        }

        sql.push_str(&format!(
            " ORDER BY bm25(episodic_fts) LIMIT ?{}",
            bindings.len() + 1
        ));
        bindings.push(Value::from(i64::from(limit)));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| AgentOSError::StorageError(format!("FTS Prepare error: {}", e)))?;

        let episode_iter = stmt
            .query_map(params_from_iter(bindings.iter()), Self::row_to_episode)
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to search agent episodes: {}", e))
            })?;

        let mut episodes = Vec::new();
        for row in episode_iter {
            episodes.push(row.map_err(|e| {
                AgentOSError::StorageError(format!("Failed to parse search result row: {}", e))
            })?);
        }

        Ok(episodes)
    }

    fn task_owned_by(&self, task_id: &TaskID, agent_id: &AgentID) -> Result<bool, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock episodic db for ownership check".to_string())
        })?;

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM episodic_events WHERE task_id = ?1 AND agent_id = ?2",
                params![
                    task_id.as_uuid().to_string(),
                    agent_id.as_uuid().to_string()
                ],
                |row| row.get(0),
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to verify task ownership: {}", e))
            })?;

        Ok(count > 0)
    }

    fn to_u32_limit(limit: usize) -> u32 {
        limit.min(u32::MAX as usize) as u32
    }

    fn to_i64_limit(limit: usize) -> i64 {
        limit.min(i64::MAX as usize) as i64
    }

    /// Expected column order:
    /// id, task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id
    fn row_to_episode(row: &rusqlite::Row) -> SqliteResult<EpisodicEntry> {
        let task_id_str: String = row.get(1)?;
        let agent_id_str: String = row.get(2)?;
        let entry_type_str: String = row.get(3)?;
        let summary: Option<String> = row.get(5)?;
        let trace_id_str: String = row.get(8)?;

        let metadata_str: Option<String> = row.get(6)?;
        let metadata =
            metadata_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

        let timestamp_str: String = row.get(7)?;
        let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
            .unwrap_or_else(|_| chrono::Local::now().into())
            .with_timezone(&Utc);

        Ok(EpisodicEntry {
            id: row.get(0)?,
            task_id: TaskID::from_uuid(Uuid::parse_str(&task_id_str).unwrap_or_default()),
            agent_id: AgentID::from_uuid(Uuid::parse_str(&agent_id_str).unwrap_or_default()),
            entry_type: EpisodeType::from_str(&entry_type_str).unwrap_or(EpisodeType::SystemEvent),
            content: row.get(4)?,
            summary,
            metadata,
            timestamp,
            trace_id: TraceID::from_uuid(Uuid::parse_str(&trace_id_str).unwrap_or_default()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_episodic_memory_record_and_query() {
        let dir = TempDir::new().unwrap();
        let mem = EpisodicStore::open(dir.path()).unwrap();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();
        let trace_id = TraceID::new();

        mem.record(
            &task_id,
            &agent_id,
            EpisodeType::UserPrompt,
            "Hello",
            Some("User greeted"),
            None,
            &trace_id,
        )
        .unwrap();
        mem.record(
            &task_id,
            &agent_id,
            EpisodeType::LLMResponse,
            "Hi there container",
            Some("LLM responded with greeting"),
            None,
            &trace_id,
        )
        .unwrap();

        let episodes = mem.timeline_by_task(&task_id, 10).unwrap();
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0].content, "Hello");
        assert_eq!(episodes[1].content, "Hi there container");

        let search_results = mem
            .search_events("container", Some(&task_id), None, 5)
            .unwrap();
        assert_eq!(search_results.len(), 1);
        assert_eq!(search_results[0].content, "Hi there container");
    }

    #[test]
    fn test_episodic_fts_finds_tool_call() {
        let dir = TempDir::new().unwrap();
        let store = EpisodicStore::open(dir.path()).unwrap();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();
        let trace_id = TraceID::new();

        store
            .record(
                &task_id,
                &agent_id,
                EpisodeType::ToolCall,
                r#"{"tool":"file-reader","path":"report.txt"}"#,
                Some("Called file-reader for report.txt"),
                None,
                &trace_id,
            )
            .unwrap();

        let results = store
            .search_events("file-reader", Some(&task_id), None, 10)
            .unwrap();
        assert!(!results.is_empty());
        assert!(results[0]
            .summary
            .as_deref()
            .unwrap()
            .contains("file-reader"));
    }

    #[test]
    fn test_recall_task_denies_other_agent() {
        let dir = TempDir::new().unwrap();
        let store = EpisodicStore::open(dir.path()).unwrap();
        let task_id = TaskID::new();
        let owner = AgentID::new();
        let other = AgentID::new();
        let trace_id = TraceID::new();

        store
            .record(
                &task_id,
                &owner,
                EpisodeType::UserPrompt,
                "hello",
                Some("prompt"),
                None,
                &trace_id,
            )
            .unwrap();

        let err = store
            .recall_task(&task_id, &other, "hello", 5)
            .expect_err("expected permission denied for non-owner agent");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }
}
