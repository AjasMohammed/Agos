use crate::types::{EpisodeType, EpisodicEntry};
use agentos_types::{AgentID, AgentOSError, PermissionOp, TaskID, TraceID};
use chrono::{DateTime, Utc};
use rusqlite::{
    params, params_from_iter, types::Value, Connection, OptionalExtension, Result as SqliteResult,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub struct EpisodicStore {
    db: Arc<Mutex<Connection>>,
}

pub struct EpisodeRecordInput<'a> {
    pub task_id: &'a TaskID,
    pub agent_id: &'a AgentID,
    pub entry_type: EpisodeType,
    pub content: &'a str,
    pub summary: Option<&'a str>,
    pub metadata: Option<serde_json::Value>,
    pub trace_id: &'a TraceID,
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
            db: Arc::new(Mutex::new(conn)),
        })
    }

    /// Record a new episodic entry. Offloads the SQLite write to the blocking thread pool.
    pub async fn record(&self, input: EpisodeRecordInput<'_>) -> Result<(), AgentOSError> {
        // Convert borrowed fields to owned so they can be moved into spawn_blocking.
        let task_id_str = input.task_id.as_uuid().to_string();
        let agent_id_str = input.agent_id.as_uuid().to_string();
        let entry_type_str = input.entry_type.as_str().to_string();
        let content_str = input.content.to_string();
        let summary_str = input.summary.map(|s| s.to_string());
        let metadata = input.metadata;
        let trace_id_str = input.trace_id.as_uuid().to_string();

        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for writing".to_string())
            })?;
            let timestamp = Utc::now().to_rfc3339();
            let metadata_str = metadata.map(|v| v.to_string());

            conn.execute(
                "INSERT INTO episodic_events (task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    task_id_str,
                    agent_id_str,
                    entry_type_str,
                    content_str,
                    summary_str,
                    metadata_str,
                    timestamp,
                    trace_id_str
                ],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to record episode: {}", e))
            })?;

            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Record task panicked: {}", e)))?
    }

    /// Record an already-materialized episodic entry object.
    pub async fn record_entry(&self, entry: EpisodicEntry) -> Result<(), AgentOSError> {
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
        self.record(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type,
            content: &content,
            summary: summary.as_deref(),
            metadata,
            trace_id: &trace_id,
        })
        .await
    }

    /// Retrieve raw timeline of a task chronologically. Offloads to blocking thread pool.
    pub async fn timeline_by_task(
        &self,
        task_id: &TaskID,
        limit: u32,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let db = self.db.clone();
        let task_id_str = task_id.as_uuid().to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for reading".to_string())
            })?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id
                     FROM episodic_events WHERE task_id = ?1 ORDER BY timestamp DESC LIMIT ?2",
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to prepare query: {}", e))
                })?;

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
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Timeline task panicked: {}", e)))?
    }

    /// Get complete history for a task in chronological order. Offloads to blocking thread pool.
    pub async fn task_history(&self, task_id: &TaskID) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let db = self.db.clone();
        let task_id_str = task_id.as_uuid().to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for reading".to_string())
            })?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id
                     FROM episodic_events WHERE task_id = ?1 ORDER BY timestamp ASC LIMIT 10000",
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to prepare query: {}", e))
                })?;

            let episode_iter = stmt
                .query_map(params![task_id_str], Self::row_to_episode)
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
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Task history task panicked: {}", e)))?
    }

    /// Full-text search within a task's event history.
    /// Caller must own the task (same agent_id observed in task events).
    pub async fn recall_task(
        &self,
        task_id: &TaskID,
        caller_agent_id: &AgentID,
        query: &str,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        if !self.task_owned_by(task_id, caller_agent_id).await? {
            return Err(AgentOSError::PermissionDenied {
                resource: format!("memory.episodic.task:{}", task_id),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        self.recall_task_with_permission(task_id, query, limit)
            .await
    }

    /// Full-text search within a task's history when caller permission has already been validated.
    pub async fn recall_task_with_permission(
        &self,
        task_id: &TaskID,
        query: &str,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        self.search_events(query, Some(task_id), None, Self::to_u32_limit(limit))
            .await
    }

    /// Search across all tasks. Permission checks are expected at the caller boundary.
    /// Offloads to blocking thread pool.
    pub async fn recall_global(
        &self,
        query: &str,
        agent_id: Option<&AgentID>,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let db = self.db.clone();
        let sanitized_query = format!("\"{}\"", query.replace('"', "\"\""));
        let agent_id_str = agent_id.map(|a| a.as_uuid().to_string());
        let limit_val = Self::to_i64_limit(limit);

        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for search".to_string())
            })?;

            let mut sql = String::from(
                "SELECT e.id, e.task_id, e.agent_id, e.entry_type, e.content, e.summary, e.metadata, e.timestamp, e.trace_id
                 FROM episodic_fts f
                 JOIN episodic_events e ON f.rowid = e.id
                 WHERE episodic_fts MATCH ?1",
            );
            let mut bindings: Vec<Value> = vec![Value::from(sanitized_query)];

            if let Some(ref a) = agent_id_str {
                sql.push_str(&format!(" AND e.agent_id = ?{}", bindings.len() + 1));
                bindings.push(Value::from(a.clone()));
            }

            if let Some(ts) = since {
                sql.push_str(&format!(" AND e.timestamp >= ?{}", bindings.len() + 1));
                bindings.push(Value::from(ts.to_rfc3339()));
            }

            sql.push_str(&format!(
                " ORDER BY e.timestamp DESC LIMIT ?{}",
                bindings.len() + 1
            ));
            bindings.push(Value::from(limit_val));

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| AgentOSError::StorageError(format!("FTS Prepare error: {}", e)))?;

            let episode_iter = stmt
                .query_map(params_from_iter(bindings.iter()), Self::row_to_episode)
                .map_err(|e| {
                    AgentOSError::StorageError(format!(
                        "Failed to search global episodes: {}",
                        e
                    ))
                })?;

            let mut episodes = Vec::new();
            for row in episode_iter {
                episodes.push(row.map_err(|e| {
                    AgentOSError::StorageError(format!(
                        "Failed to parse search result row: {}",
                        e
                    ))
                })?);
            }

            Ok(episodes)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Recall global task panicked: {}", e)))?
    }

    /// Recall past events via FTS5 search. Offloads to blocking thread pool.
    pub async fn search_events(
        &self,
        query: &str,
        filter_task: Option<&TaskID>,
        filter_agent: Option<&AgentID>,
        limit: u32,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let db = self.db.clone();
        let sanitized_query = format!("\"{}\"", query.replace('"', "\"\""));
        let filter_task_str = filter_task.map(|t| t.as_uuid().to_string());
        let filter_agent_str = filter_agent.map(|a| a.as_uuid().to_string());

        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for search".to_string())
            })?;

            let mut sql = String::from(
                "SELECT e.id, e.task_id, e.agent_id, e.entry_type, e.content, e.summary, e.metadata, e.timestamp, e.trace_id
                 FROM episodic_fts f
                 JOIN episodic_events e ON f.rowid = e.id
                 WHERE episodic_fts MATCH ?1",
            );
            let mut bindings: Vec<Value> = vec![Value::from(sanitized_query)];

            if let Some(ref t) = filter_task_str {
                sql.push_str(&format!(" AND e.task_id = ?{}", bindings.len() + 1));
                bindings.push(Value::from(t.clone()));
            }

            if let Some(ref a) = filter_agent_str {
                sql.push_str(&format!(" AND e.agent_id = ?{}", bindings.len() + 1));
                bindings.push(Value::from(a.clone()));
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
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Search events task panicked: {}", e)))?
    }

    /// Find successful `SystemEvent` episodes since an optional timestamp.
    ///
    /// This is used by consolidation pipelines to detect repeated successful patterns.
    /// Offloads to blocking thread pool.
    pub async fn find_successful_episodes(
        &self,
        since: Option<DateTime<Utc>>,
        limit: u32,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError(
                    "Failed to lock episodic db for pattern query".to_string(),
                )
            })?;
            let since_str = since.unwrap_or(DateTime::<Utc>::MIN_UTC).to_rfc3339();
            let mut stmt = conn
                .prepare(
                    "SELECT id, task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id
                     FROM episodic_events
                     WHERE entry_type = ?1
                       AND metadata LIKE '%\"outcome\":\"success\"%'
                       AND timestamp >= ?2
                     ORDER BY timestamp ASC
                     LIMIT ?3",
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!(
                        "Failed to prepare successful episodes query: {}",
                        e
                    ))
                })?;

            let rows = stmt
                .query_map(
                    params![EpisodeType::SystemEvent.as_str(), since_str, limit],
                    Self::row_to_episode,
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!(
                        "Failed to query successful episodes: {}",
                        e
                    ))
                })?;

            let mut episodes = Vec::new();
            for row in rows {
                episodes.push(row.map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to parse episode row: {}", e))
                })?);
            }
            Ok(episodes)
        })
        .await
        .map_err(|e| {
            AgentOSError::StorageError(format!("Find successful episodes task panicked: {}", e))
        })?
    }

    async fn task_owned_by(
        &self,
        task_id: &TaskID,
        agent_id: &AgentID,
    ) -> Result<bool, AgentOSError> {
        let db = self.db.clone();
        let task_id_str = task_id.as_uuid().to_string();
        let agent_id_str = agent_id.as_uuid().to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError(
                    "Failed to lock episodic db for ownership check".to_string(),
                )
            })?;

            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(1) FROM episodic_events WHERE task_id = ?1 AND agent_id = ?2",
                    params![task_id_str, agent_id_str],
                    |row| row.get(0),
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to verify task ownership: {}", e))
                })?;

            Ok(count > 0)
        })
        .await
        .map_err(|e| {
            AgentOSError::StorageError(format!("Task ownership check task panicked: {}", e))
        })?
    }

    fn to_u32_limit(limit: usize) -> u32 {
        limit.min(u32::MAX as usize) as u32
    }

    fn to_i64_limit(limit: usize) -> i64 {
        limit.min(i64::MAX as usize) as i64
    }

    /// Delete a single episodic entry by its row id. Offloads to blocking thread pool.
    pub async fn delete(&self, id: i64) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for delete".to_string())
            })?;
            let deleted = conn
                .execute("DELETE FROM episodic_events WHERE id = ?1", params![id])
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to delete episode: {}", e))
                })?;
            if deleted == 0 {
                return Err(AgentOSError::StorageError(format!(
                    "Episodic entry {} not found",
                    id
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Delete task panicked: {}", e)))?
    }

    /// Count episodic events, optionally scoped to an agent. Offloads to blocking thread pool.
    pub async fn count(&self, agent_id: Option<&AgentID>) -> Result<usize, AgentOSError> {
        let db = self.db.clone();
        let agent_id_str = agent_id.map(|id| id.as_uuid().to_string());
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for count".to_string())
            })?;
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM episodic_events WHERE (?1 IS NULL OR agent_id IS NULL OR agent_id = ?1)",
                    params![agent_id_str],
                    |row| row.get(0),
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Count query failed: {}", e))
                })?;
            Ok(count as usize)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Count task panicked: {}", e)))?
    }

    /// Archival sweep for Tier 3 persistent episodic memory (Spec §11).
    /// Offloads to blocking thread pool.
    pub async fn sweep_old_entries(
        &self,
        max_age: std::time::Duration,
    ) -> Result<usize, AgentOSError> {
        let chrono_age = chrono::Duration::from_std(max_age)
            .map_err(|e| AgentOSError::StorageError(format!("Invalid max_age duration: {}", e)))?;
        let cutoff = (Utc::now() - chrono_age).to_rfc3339();
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for sweep".to_string())
            })?;

            let deleted = conn
                .execute(
                    "DELETE FROM episodic_events WHERE timestamp < ?1",
                    params![cutoff],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to sweep old episodes: {}", e))
                })?;

            Ok(deleted)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Sweep task panicked: {}", e)))?
    }

    /// Export all episodic events as newline-delimited JSON (JSONL) to the given writer.
    ///
    /// Each line is a JSON object with fields: id, task_id, agent_id, entry_type,
    /// content, summary, metadata, timestamp, trace_id.
    ///
    /// Note: This is a synchronous batch utility. Do not call from hot-path async code.
    pub fn export_jsonl<W: std::io::Write>(&self, writer: &mut W) -> Result<usize, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock episodic db for export".to_string())
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id
                 FROM episodic_events ORDER BY timestamp ASC",
            )
            .map_err(|e| AgentOSError::StorageError(format!("Export prepare error: {}", e)))?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let task_id: String = row.get(1)?;
                let agent_id: String = row.get(2)?;
                let entry_type: String = row.get(3)?;
                let content: String = row.get(4)?;
                let summary: Option<String> = row.get(5)?;
                let metadata: Option<String> = row.get(6)?;
                let timestamp: String = row.get(7)?;
                let trace_id: String = row.get(8)?;
                Ok(serde_json::json!({
                    "id": id,
                    "task_id": task_id,
                    "agent_id": agent_id,
                    "entry_type": entry_type,
                    "content": content,
                    "summary": summary,
                    "metadata": metadata.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "timestamp": timestamp,
                    "trace_id": trace_id,
                }))
            })
            .map_err(|e| AgentOSError::StorageError(format!("Export query error: {}", e)))?;

        let mut count = 0;
        for row in rows {
            let value =
                row.map_err(|e| AgentOSError::StorageError(format!("Export row error: {}", e)))?;
            serde_json::to_writer(&mut *writer, &value).map_err(|e| {
                AgentOSError::StorageError(format!("Export serialization error: {}", e))
            })?;
            writeln!(writer)
                .map_err(|e| AgentOSError::StorageError(format!("Export write error: {}", e)))?;
            count += 1;
        }

        Ok(count)
    }

    /// Import episodic events from newline-delimited JSON (JSONL).
    ///
    /// Each line must be a JSON object with fields: task_id, agent_id, entry_type,
    /// content, trace_id. Optional: summary, metadata, timestamp.
    ///
    /// Returns the number of entries imported.
    pub async fn import_jsonl<R: std::io::BufRead>(
        &self,
        reader: R,
    ) -> Result<usize, AgentOSError> {
        // Parse all lines into owned records in the async context (no DB lock held).
        type Record = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
        );
        let mut records: Vec<Record> = Vec::new();

        for (line_num, line) in reader.lines().enumerate() {
            let line_num = line_num + 1;
            let line = line.map_err(|e| {
                AgentOSError::StorageError(format!("Failed to read JSONL line: {}", e))
            })?;
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let obj: serde_json::Value = serde_json::from_str(&line).map_err(|e| {
                AgentOSError::StorageError(format!("Invalid JSON on line {}: {}", line_num, e))
            })?;

            let task_id = obj["task_id"]
                .as_str()
                .ok_or_else(|| {
                    AgentOSError::StorageError(format!(
                        "Missing 'task_id' field on line {}",
                        line_num
                    ))
                })?
                .to_owned();
            Uuid::parse_str(&task_id).map_err(|_| {
                AgentOSError::StorageError(format!(
                    "Invalid task_id UUID '{}' on line {}",
                    task_id, line_num
                ))
            })?;

            let agent_id = obj["agent_id"]
                .as_str()
                .ok_or_else(|| {
                    AgentOSError::StorageError(format!(
                        "Missing 'agent_id' field on line {}",
                        line_num
                    ))
                })?
                .to_owned();
            Uuid::parse_str(&agent_id).map_err(|_| {
                AgentOSError::StorageError(format!(
                    "Invalid agent_id UUID '{}' on line {}",
                    agent_id, line_num
                ))
            })?;

            let entry_type = obj["entry_type"]
                .as_str()
                .unwrap_or("system_event")
                .to_owned();
            let content = obj["content"]
                .as_str()
                .ok_or_else(|| {
                    AgentOSError::StorageError(format!(
                        "Missing 'content' field on line {}",
                        line_num
                    ))
                })?
                .to_owned();
            let summary = obj["summary"].as_str().map(|s| s.to_owned());
            let metadata = obj["metadata"]
                .as_object()
                .map(|m| serde_json::to_string(m).unwrap_or_default());
            let timestamp = obj["timestamp"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| Utc::now().to_rfc3339());

            let trace_id = obj["trace_id"]
                .as_str()
                .ok_or_else(|| {
                    AgentOSError::StorageError(format!(
                        "Missing 'trace_id' field on line {}",
                        line_num
                    ))
                })?
                .to_owned();
            Uuid::parse_str(&trace_id).map_err(|_| {
                AgentOSError::StorageError(format!(
                    "Invalid trace_id UUID '{}' on line {}",
                    trace_id, line_num
                ))
            })?;

            records.push((
                task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id,
            ));
        }

        let count = records.len();
        if count == 0 {
            return Ok(0);
        }

        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for import".to_string())
            })?;
            // Wrap all inserts in a single transaction: faster (one fsync) and atomic
            // (no partial imports if a later row fails).
            let tx = conn.transaction().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to begin import transaction: {}", e))
            })?;

            for (task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id) in
                &records
            {
                tx.execute(
                    "INSERT INTO episodic_events (task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Import insert failed: {}", e))
                })?;
            }

            tx.commit().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to commit import transaction: {}", e))
            })?;
            Ok(count)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Import task panicked: {}", e)))?
    }

    /// Fetch a single episodic entry by its integer ID.
    pub async fn get_by_id(&self, id: i64) -> Result<Option<EpisodicEntry>, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock episodic db for read".to_string())
            })?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, task_id, agent_id, entry_type, content, summary, metadata, timestamp, trace_id
                     FROM episodic_events WHERE id = ?1",
                )
                .map_err(|e| AgentOSError::StorageError(format!("Prepare failed: {}", e)))?;
            let entry = stmt
                .query_row(params![id], Self::row_to_episode)
                .optional()
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Episodic get_by_id failed: {}", e))
                })?;
            Ok(entry)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("get_by_id task panicked: {}", e)))?
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
            entry_type: entry_type_str.parse().unwrap_or(EpisodeType::SystemEvent),
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

    #[tokio::test]
    async fn test_episodic_memory_record_and_query() {
        let dir = TempDir::new().unwrap();
        let mem = EpisodicStore::open(dir.path()).unwrap();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();
        let trace_id = TraceID::new();

        mem.record(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type: EpisodeType::UserPrompt,
            content: "Hello",
            summary: Some("User greeted"),
            metadata: None,
            trace_id: &trace_id,
        })
        .await
        .unwrap();
        mem.record(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type: EpisodeType::LLMResponse,
            content: "Hi there container",
            summary: Some("LLM responded with greeting"),
            metadata: None,
            trace_id: &trace_id,
        })
        .await
        .unwrap();

        let episodes = mem.timeline_by_task(&task_id, 10).await.unwrap();
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0].content, "Hello");
        assert_eq!(episodes[1].content, "Hi there container");

        let search_results = mem
            .search_events("container", Some(&task_id), None, 5)
            .await
            .unwrap();
        assert_eq!(search_results.len(), 1);
        assert_eq!(search_results[0].content, "Hi there container");
    }

    #[tokio::test]
    async fn test_episodic_fts_finds_tool_call() {
        let dir = TempDir::new().unwrap();
        let store = EpisodicStore::open(dir.path()).unwrap();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();
        let trace_id = TraceID::new();

        store
            .record(EpisodeRecordInput {
                task_id: &task_id,
                agent_id: &agent_id,
                entry_type: EpisodeType::ToolCall,
                content: r#"{"tool":"file-reader","path":"report.txt"}"#,
                summary: Some("Called file-reader for report.txt"),
                metadata: None,
                trace_id: &trace_id,
            })
            .await
            .unwrap();

        let results = store
            .search_events("file-reader", Some(&task_id), None, 10)
            .await
            .unwrap();
        assert!(!results.is_empty());
        assert!(results[0]
            .summary
            .as_deref()
            .unwrap()
            .contains("file-reader"));
    }

    #[tokio::test]
    async fn test_recall_task_denies_other_agent() {
        let dir = TempDir::new().unwrap();
        let store = EpisodicStore::open(dir.path()).unwrap();
        let task_id = TaskID::new();
        let owner = AgentID::new();
        let other = AgentID::new();
        let trace_id = TraceID::new();

        store
            .record(EpisodeRecordInput {
                task_id: &task_id,
                agent_id: &owner,
                entry_type: EpisodeType::UserPrompt,
                content: "hello",
                summary: Some("prompt"),
                metadata: None,
                trace_id: &trace_id,
            })
            .await
            .unwrap();

        let err = store
            .recall_task(&task_id, &other, "hello", 5)
            .await
            .expect_err("expected permission denied for non-owner agent");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }
}
