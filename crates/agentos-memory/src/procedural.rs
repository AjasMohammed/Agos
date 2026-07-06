use crate::embedder::Embedder;
use crate::types::{Procedure, ProcedureSearchResult, ProcedureStep};
use agentos_types::{AgentID, AgentOSError};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const EXPECTED_EMBEDDING_DIMENSION: usize = 384;

pub struct ProceduralStore {
    conn: Arc<Mutex<Connection>>,
    embedder: Arc<Embedder>,
    dimension: usize,
}

impl ProceduralStore {
    /// Open procedural memory using the default model cache dir under `{data_dir}/models`.
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError> {
        Self::open_with_cache_dir(data_dir, &data_dir.join("models"))
    }

    /// Open procedural memory with an explicit embedding model cache directory.
    pub fn open_with_cache_dir(
        data_dir: &Path,
        model_cache_dir: &Path,
    ) -> Result<Self, AgentOSError> {
        let embedder = Arc::new(Embedder::with_cache_dir(model_cache_dir).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to initialize embedder: {}", e))
        })?);
        Self::open_with_embedder(data_dir, embedder)
    }

    /// Open procedural memory with a caller-provided embedder (for testing / shared embedder).
    pub fn open_with_embedder(
        data_dir: &Path,
        embedder: Arc<Embedder>,
    ) -> Result<Self, AgentOSError> {
        let db_path = data_dir.join("procedural_memory.db");
        let conn = Connection::open(&db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open procedural memory DB: {}", e))
        })?;
        crate::restrict_db_permissions(&db_path);

        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS procedures (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                description     TEXT NOT NULL,
                preconditions   TEXT NOT NULL,
                steps           TEXT NOT NULL,
                postconditions  TEXT NOT NULL,
                success_count   INTEGER NOT NULL DEFAULT 0,
                failure_count   INTEGER NOT NULL DEFAULT 0,
                source_episodes TEXT NOT NULL,
                agent_id        TEXT,
                tags            TEXT NOT NULL,
                embedding       BLOB NOT NULL,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_proc_agent ON procedures(agent_id);
            CREATE INDEX IF NOT EXISTS idx_proc_name ON procedures(name);
            CREATE INDEX IF NOT EXISTS idx_proc_updated ON procedures(updated_at);

            CREATE TABLE IF NOT EXISTS procedures_fts_content (
                rowid       INTEGER PRIMARY KEY AUTOINCREMENT,
                proc_id     TEXT NOT NULL UNIQUE,
                name        TEXT NOT NULL,
                description TEXT NOT NULL,
                steps_text  TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS procedures_fts USING fts5(
                name, description, steps_text,
                content='procedures_fts_content',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS proc_fts_ai AFTER INSERT ON procedures_fts_content BEGIN
              INSERT INTO procedures_fts(rowid, name, description, steps_text)
                VALUES (new.rowid, new.name, new.description, new.steps_text);
            END;
            CREATE TRIGGER IF NOT EXISTS proc_fts_ad AFTER DELETE ON procedures_fts_content BEGIN
              INSERT INTO procedures_fts(procedures_fts, rowid, name, description, steps_text)
                VALUES('delete', old.rowid, old.name, old.description, old.steps_text);
            END;
            CREATE TRIGGER IF NOT EXISTS proc_fts_au AFTER UPDATE ON procedures_fts_content BEGIN
              INSERT INTO procedures_fts(procedures_fts, rowid, name, description, steps_text)
                VALUES('delete', old.rowid, old.name, old.description, old.steps_text);
              INSERT INTO procedures_fts(rowid, name, description, steps_text)
                VALUES (new.rowid, new.name, new.description, new.steps_text);
            END;
        ",
        )
        .map_err(|e| {
            AgentOSError::StorageError(format!("Failed to init procedural memory tables: {}", e))
        })?;

        crate::lifecycle::migrate_lifecycle_columns(&conn, "procedures")?;

        let probe = embedder
            .embed(&["procedural-memory-dimension-probe"])
            .map_err(|e| {
                AgentOSError::StorageError(format!("Embedding dimension probe failed: {}", e))
            })?;
        let dimension = probe.first().map(|v| v.len()).ok_or_else(|| {
            AgentOSError::StorageError("Embedding model returned empty probe result".to_string())
        })?;
        if dimension != EXPECTED_EMBEDDING_DIMENSION {
            return Err(AgentOSError::StorageError(format!(
                "Unexpected embedding dimension {} (expected {})",
                dimension, EXPECTED_EMBEDDING_DIMENSION
            )));
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embedder,
            dimension,
        })
    }

    fn build_steps_text(steps: &[ProcedureStep]) -> String {
        steps
            .iter()
            .map(|s| {
                format!(
                    "{}: {} {} {}",
                    s.order,
                    s.action,
                    s.tool.clone().unwrap_or_default(),
                    s.expected_outcome.clone().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn build_embedding_text(procedure: &Procedure) -> String {
        [
            procedure.name.as_str(),
            procedure.description.as_str(),
            &procedure.preconditions.join("\n"),
            &Self::build_steps_text(&procedure.steps),
            &procedure.postconditions.join("\n"),
            &procedure.tags.join("\n"),
        ]
        .join("\n")
    }

    /// Store a procedure. Both embedding computation and SQLite writes are offloaded
    /// to the blocking thread pool via `spawn_blocking` so async worker threads are
    /// never blocked by ONNX model inference or disk I/O.
    /// Uses `conn.transaction()` for atomic, auto-rollback-on-error semantics.
    pub async fn store(&self, procedure: &Procedure) -> Result<String, AgentOSError> {
        let proc_id = if procedure.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            procedure.id.clone()
        };
        let now = Utc::now().to_rfc3339();
        let embedding_text = Self::build_embedding_text(procedure);
        let preconditions = serde_json::to_string(&procedure.preconditions).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to serialize preconditions: {}", e))
        })?;
        let steps = serde_json::to_string(&procedure.steps)
            .map_err(|e| AgentOSError::StorageError(format!("Failed to serialize steps: {}", e)))?;
        let postconditions = serde_json::to_string(&procedure.postconditions).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to serialize postconditions: {}", e))
        })?;
        let source_episodes = serde_json::to_string(&procedure.source_episodes).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to serialize source episodes: {}", e))
        })?;
        let tags = serde_json::to_string(&procedure.tags)
            .map_err(|e| AgentOSError::StorageError(format!("Failed to serialize tags: {}", e)))?;
        let agent_id_str = procedure.agent_id.map(|id| id.as_uuid().to_string());
        let created_at = if procedure.created_at.timestamp() == 0 {
            now.clone()
        } else {
            procedure.created_at.to_rfc3339()
        };
        let steps_text = Self::build_steps_text(&procedure.steps);
        let name = procedure.name.clone();
        let description = procedure.description.clone();
        let success_count = procedure.success_count;
        let failure_count = procedure.failure_count;
        let last_used_at = procedure.last_used_at.map(|t| t.to_rfc3339());
        let use_count = procedure.use_count;
        let confidence = procedure.confidence as f64;
        let status = procedure.status.as_str();

        let embedder = self.embedder.clone();
        let dimension = self.dimension;
        let db = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            // Embedding is CPU-intensive; run it here on the blocking thread pool
            // so async worker threads are not blocked by ONNX model inference.
            let embedding = embedder
                .embed(&[embedding_text.as_str()])
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to compute embedding: {}", e))
                })?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    AgentOSError::StorageError(
                        "Procedure embedding returned empty result".to_string(),
                    )
                })?;
            if embedding.len() != dimension {
                return Err(AgentOSError::StorageError(format!(
                    "Procedure embedding dimension mismatch: expected {}, got {}",
                    dimension,
                    embedding.len()
                )));
            }
            let mut blob = Vec::with_capacity(embedding.len() * 4);
            for val in embedding {
                blob.extend_from_slice(&val.to_le_bytes());
            }

            let mut conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock procedural db for store".to_string())
            })?;

            // Use rusqlite's Transaction for automatic rollback on any early return.
            let tx = conn.transaction().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to begin transaction: {}", e))
            })?;

            tx.execute(
                "INSERT OR REPLACE INTO procedures (
                    id, name, description, preconditions, steps, postconditions,
                    success_count, failure_count, source_episodes, agent_id, tags,
                    embedding, created_at, updated_at,
                    last_used_at, use_count, confidence, status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                           ?15, ?16, ?17, ?18)",
                params![
                    proc_id,
                    name,
                    description,
                    preconditions,
                    steps,
                    postconditions,
                    success_count,
                    failure_count,
                    source_episodes,
                    agent_id_str,
                    tags,
                    blob,
                    created_at,
                    now,
                    last_used_at,
                    use_count,
                    confidence,
                    status
                ],
            )
            .map_err(|e| AgentOSError::StorageError(format!("Failed to store procedure: {}", e)))?;

            tx.execute(
                "INSERT INTO procedures_fts_content (proc_id, name, description, steps_text)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(proc_id) DO UPDATE SET
                    name=excluded.name,
                    description=excluded.description,
                    steps_text=excluded.steps_text",
                params![proc_id, name, description, steps_text],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to write procedure FTS content: {}", e))
            })?;

            tx.commit().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to commit store transaction: {}", e))
            })?;

            Ok(proc_id)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Store task panicked: {}", e)))?
    }

    /// Search procedures using hybrid FTS5 + vector similarity.
    /// Embedding is computed in the async context; SQLite work is offloaded via `spawn_blocking`.
    pub async fn search(
        &self,
        query: &str,
        agent_id: Option<&AgentID>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<ProcedureSearchResult>, AgentOSError> {
        if !(0.0..=1.0).contains(&min_score) {
            return Err(AgentOSError::SchemaValidation(format!(
                "min_score must be between 0.0 and 1.0, got {}",
                min_score
            )));
        }

        if top_k == 0 {
            return Ok(Vec::new());
        }

        let agent_id_str = agent_id.map(|id| id.as_uuid().to_string());
        let sanitized_query = format!("\"{}\"", query.replace('"', "\"\""));
        let query_owned = query.to_owned();
        let dimension = self.dimension;
        let embedder = self.embedder.clone();
        let db = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            // Embedding is CPU-intensive; run it here on the blocking thread pool
            // so async worker threads are not blocked by ONNX model inference.
            let query_embedding = embedder
                .embed(&[query_owned.as_str()])
                .map_err(|e| AgentOSError::StorageError(format!("Query embed error: {}", e)))?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    AgentOSError::StorageError("Query embedding returned empty result".to_string())
                })?;
            if query_embedding.len() != dimension {
                return Err(AgentOSError::StorageError(format!(
                    "Query embedding dimension mismatch: expected {}, got {}",
                    dimension,
                    query_embedding.len()
                )));
            }

            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock procedural db for search".to_string())
            })?;

            let fts_ranks: HashMap<i64, f32> = {
                let mut map = HashMap::new();
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT rowid, rank FROM procedures_fts
                     WHERE procedures_fts MATCH ?1
                     ORDER BY rank
                     LIMIT 200",
                ) {
                    if let Ok(rows) = stmt.query_map(params![sanitized_query], |row| {
                        let rowid: i64 = row.get(0)?;
                        let rank: f64 = row.get(1)?;
                        Ok((rowid, rank as f32))
                    }) {
                        for row in rows.flatten() {
                            map.insert(row.0, row.1);
                        }
                    }
                }
                map
            };

            let use_fts = !fts_ranks.is_empty();

            // Shared row mapper used by both the FTS path and the fallback path.
            let map_proc_row =
                |row: &rusqlite::Row<'_>| -> rusqlite::Result<(Procedure, Vec<f32>, i64)> {
                    let id: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    let description: String = row.get(2)?;
                    let preconditions_json: String = row.get(3)?;
                    let steps_json: String = row.get(4)?;
                    let postconditions_json: String = row.get(5)?;
                    let success_count: u32 = row.get(6)?;
                    let failure_count: u32 = row.get(7)?;
                    let source_episodes_json: String = row.get(8)?;
                    let aid_str: Option<String> = row.get(9)?;
                    let tags_json: String = row.get(10)?;
                    let created_at: String = row.get(11)?;
                    let updated_at: String = row.get(12)?;
                    let blob: Vec<u8> = row.get(13)?;
                    let rowid: i64 = row.get(14)?;
                    let (last_used_at, use_count, confidence, status) =
                        crate::lifecycle::lifecycle_from_row(row, 15)?;

                    let mut embedding = Vec::with_capacity(blob.len() / 4);
                    for bytes in blob.chunks_exact(4) {
                        embedding
                            .push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                    }

                    let procedure = Procedure {
                        id,
                        name,
                        description,
                        preconditions: serde_json::from_str(&preconditions_json)
                            .unwrap_or_default(),
                        steps: serde_json::from_str(&steps_json).unwrap_or_default(),
                        postconditions: serde_json::from_str(&postconditions_json)
                            .unwrap_or_default(),
                        success_count,
                        failure_count,
                        source_episodes: serde_json::from_str(&source_episodes_json)
                            .unwrap_or_default(),
                        agent_id: aid_str
                            .and_then(|s| Uuid::parse_str(&s).ok())
                            .map(AgentID::from_uuid),
                        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
                            .unwrap_or_else(|_| chrono::Local::now().into())
                            .with_timezone(&Utc),
                        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at)
                            .unwrap_or_else(|_| chrono::Local::now().into())
                            .with_timezone(&Utc),
                        last_used_at,
                        use_count,
                        confidence,
                        status,
                    };

                    Ok((procedure, embedding, rowid))
                };

            // Build the candidate query using parameterized ?N placeholders so that
            // rowid values (which come from a prior SQLite FTS5 query, not user input)
            // follow the same parameterized-query convention as the rest of the codebase.
            let raw_rows: Vec<(Procedure, Vec<f32>, i64)> = if use_fts {
                let rowids: Vec<i64> = fts_ranks.keys().copied().collect();
                // agent_id_str is bound as ?1; rowids are bound as ?2..?N
                let placeholders = (2..=rowids.len() + 1)
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT p.id, p.name, p.description, p.preconditions, p.steps, p.postconditions,
                            p.success_count, p.failure_count, p.source_episodes, p.agent_id, p.tags,
                            p.created_at, p.updated_at, p.embedding, c.rowid,
                            p.last_used_at, p.use_count, p.confidence, p.status
                     FROM procedures p
                     JOIN procedures_fts_content c ON c.proc_id = p.id
                     WHERE c.rowid IN ({placeholders})
                       AND (?1 IS NULL OR p.agent_id IS NULL OR p.agent_id = ?1)"
                );
                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
                let mut bound: Vec<rusqlite::types::Value> = Vec::with_capacity(rowids.len() + 1);
                bound.push(match &agent_id_str {
                    Some(s) => rusqlite::types::Value::Text(s.clone()),
                    None => rusqlite::types::Value::Null,
                });
                for id in &rowids {
                    bound.push(rusqlite::types::Value::Integer(*id));
                }
                let collected = stmt
                    .query_map(rusqlite::params_from_iter(bound.iter()), map_proc_row)
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
                collected
            } else {
                let sql =
                    "SELECT p.id, p.name, p.description, p.preconditions, p.steps, p.postconditions,
                            p.success_count, p.failure_count, p.source_episodes, p.agent_id, p.tags,
                            p.created_at, p.updated_at, p.embedding, c.rowid,
                            p.last_used_at, p.use_count, p.confidence, p.status
                     FROM procedures p
                     JOIN procedures_fts_content c ON c.proc_id = p.id
                     WHERE (?1 IS NULL OR p.agent_id IS NULL OR p.agent_id = ?1)
                     ORDER BY p.updated_at DESC
                     LIMIT 200";
                let mut stmt = conn
                    .prepare(sql)
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
                let collected = stmt
                    .query_map(params![agent_id_str], map_proc_row)
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
                collected
            };

            let mut results = Vec::new();
            for (procedure, embedding, rowid) in raw_rows {
                if embedding.len() != dimension {
                    continue;
                }
                let semantic_score = Self::cosine_similarity(&query_embedding, &embedding);
                if semantic_score < min_score {
                    continue;
                }
                let fts_score = fts_ranks.get(&rowid).map(|r| -r).unwrap_or(0.0);
                let rrf_score = if use_fts && fts_score > 0.0 {
                    let fts_normalized = fts_score / (fts_score + 60.0);
                    0.7 * semantic_score + 0.3 * fts_normalized
                } else {
                    semantic_score
                };
                results.push(ProcedureSearchResult {
                    procedure,
                    semantic_score,
                    fts_score,
                    rrf_score,
                });
            }

            results.sort_by(|a, b| {
                b.rrf_score
                    .partial_cmp(&a.rrf_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            results.truncate(top_k);
            Ok(results)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Search task panicked: {}", e)))?
    }

    /// Get a procedure by ID. Offloads to blocking thread pool.
    pub async fn get(&self, id: &str) -> Result<Option<Procedure>, AgentOSError> {
        let db = self.conn.clone();
        let id_owned = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock procedural db for get".to_string())
            })?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, description, preconditions, steps, postconditions,
                            success_count, failure_count, source_episodes, agent_id, tags,
                            created_at, updated_at,
                            last_used_at, use_count, confidence, status
                     FROM procedures WHERE id = ?1",
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            let mut rows = stmt
                .query_map(params![id_owned], Self::row_to_procedure)
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            match rows.next() {
                Some(Ok(p)) => Ok(Some(p)),
                Some(Err(e)) => Err(AgentOSError::StorageError(e.to_string())),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Get task panicked: {}", e)))?
    }

    /// Mark procedures as used right now: bump `use_count` and stamp
    /// `last_used_at`. Fire-and-forget reinforcement — callers must never fail
    /// a task on a touch error. Returns the number of rows updated.
    pub async fn touch(&self, ids: &[String]) -> Result<u32, AgentOSError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let db = self.conn.clone();
        let ids = ids.to_vec();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock procedural db for touch".to_string())
            })?;
            let mut total = 0u32;
            for chunk in ids.chunks(500) {
                // now is bound as ?1; ids are bound as ?2..?N
                let placeholders = (2..=chunk.len() + 1)
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "UPDATE procedures
                     SET last_used_at = ?1, use_count = use_count + 1
                     WHERE id IN ({placeholders})"
                );
                let mut bound: Vec<rusqlite::types::Value> = Vec::with_capacity(chunk.len() + 1);
                bound.push(rusqlite::types::Value::Text(now.clone()));
                for id in chunk {
                    bound.push(rusqlite::types::Value::Text(id.clone()));
                }
                total += conn
                    .execute(&sql, rusqlite::params_from_iter(bound.iter()))
                    .map_err(|e| {
                        AgentOSError::StorageError(format!("Failed to touch procedures: {}", e))
                    })? as u32;
            }
            Ok(total)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Touch task panicked: {}", e)))?
    }

    /// Update success/failure statistics for a procedure and recompute its
    /// lifecycle confidence as the Laplace-smoothed success rate
    /// `(success + 1) / (success + failure + 2)` — smoothing keeps a single
    /// outcome from swinging a fresh procedure to 0 or 1.
    /// Offloads to blocking thread pool.
    pub async fn update_stats(&self, id: &str, success: bool) -> Result<(), AgentOSError> {
        let db = self.conn.clone();
        let id_owned = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError(
                    "Failed to lock procedural db for update_stats".to_string(),
                )
            })?;
            let now = Utc::now().to_rfc3339();
            // RHS column references read the pre-update values, so the
            // post-increment Laplace numerator is written out explicitly.
            let sql = if success {
                "UPDATE procedures
                 SET success_count = success_count + 1,
                     confidence = CAST(success_count + 2 AS REAL)
                                  / (success_count + failure_count + 3),
                     updated_at = ?2
                 WHERE id = ?1"
            } else {
                "UPDATE procedures
                 SET failure_count = failure_count + 1,
                     confidence = CAST(success_count + 1 AS REAL)
                                  / (success_count + failure_count + 3),
                     updated_at = ?2
                 WHERE id = ?1"
            };
            let updated = conn.execute(sql, params![id_owned, now]).map_err(|e| {
                AgentOSError::StorageError(format!("Failed to update procedure stats: {}", e))
            })?;
            if updated == 0 {
                return Err(AgentOSError::StorageError(format!(
                    "Procedure '{}' not found",
                    id_owned
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Update stats task panicked: {}", e)))?
    }

    /// Delete a procedure and its FTS content. Uses `conn.transaction()` for safe atomicity.
    /// Offloads to blocking thread pool.
    pub async fn delete(&self, id: &str) -> Result<(), AgentOSError> {
        let db = self.conn.clone();
        let id_owned = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock procedural db for delete".to_string())
            })?;

            // Use rusqlite's Transaction for automatic rollback on any early return.
            let tx = conn.transaction().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to begin delete transaction: {}", e))
            })?;

            tx.execute(
                "DELETE FROM procedures_fts_content WHERE proc_id = ?1",
                params![id_owned],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to delete procedure FTS content: {}", e))
            })?;

            let deleted = tx
                .execute("DELETE FROM procedures WHERE id = ?1", params![id_owned])
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to delete procedure: {}", e))
                })?;

            if deleted == 0 {
                return Err(AgentOSError::StorageError(format!(
                    "Procedure '{}' not found",
                    id_owned
                )));
            }

            tx.commit().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to commit delete transaction: {}", e))
            })?;

            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Delete task panicked: {}", e)))?
    }

    /// Delete every procedure (and its FTS rows) for the given agent. Returns the number of
    /// `procedures` rows removed. Offloads to the blocking thread pool.
    pub async fn delete_by_agent(&self, agent_id: &AgentID) -> Result<usize, AgentOSError> {
        let db = self.conn.clone();
        let agent_id_str = agent_id.as_uuid().to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().map_err(|_| {
                AgentOSError::StorageError(
                    "Failed to lock procedural db for delete_by_agent".to_string(),
                )
            })?;
            let tx = conn.transaction().map_err(|e| {
                AgentOSError::StorageError(format!(
                    "Failed to begin delete_by_agent transaction: {}",
                    e
                ))
            })?;
            tx.execute(
                "DELETE FROM procedures_fts_content WHERE proc_id IN (SELECT id FROM procedures WHERE agent_id = ?1)",
                params![agent_id_str],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!(
                    "Failed to delete procedure FTS for agent: {}",
                    e
                ))
            })?;
            let deleted = tx
                .execute(
                    "DELETE FROM procedures WHERE agent_id = ?1",
                    params![agent_id_str],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!(
                        "Failed to delete procedures for agent: {}",
                        e
                    ))
                })?;
            tx.commit().map_err(|e| {
                AgentOSError::StorageError(format!(
                    "Failed to commit delete_by_agent: {}",
                    e
                ))
            })?;
            Ok(deleted)
        })
        .await
        .map_err(|e| {
            AgentOSError::StorageError(format!("delete_by_agent task panicked: {}", e))
        })?
    }

    /// List procedures, optionally scoped to an agent. Offloads to blocking thread pool.
    pub async fn list_by_agent(
        &self,
        agent_id: Option<&AgentID>,
        limit: usize,
    ) -> Result<Vec<Procedure>, AgentOSError> {
        let db = self.conn.clone();
        let max = limit.min(i64::MAX as usize) as i64;
        let agent_id_str = agent_id.map(|id| id.as_uuid().to_string());
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError(
                    "Failed to lock procedural db for list_by_agent".to_string(),
                )
            })?;

            let mut stmt = conn
                .prepare(
                    "SELECT id, name, description, preconditions, steps, postconditions,
                            success_count, failure_count, source_episodes, agent_id, tags,
                            created_at, updated_at,
                            last_used_at, use_count, confidence, status
                     FROM procedures
                     WHERE (?1 IS NULL OR agent_id IS NULL OR agent_id = ?1)
                     ORDER BY updated_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            let rows = stmt
                .query_map(params![agent_id_str, max], Self::row_to_procedure)
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            let mut procedures = Vec::new();
            for row in rows {
                procedures.push(row.map_err(|e| AgentOSError::StorageError(e.to_string()))?);
            }
            Ok(procedures)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("List task panicked: {}", e)))?
    }

    /// Delete procedures older than `max_age`. Offloads to blocking thread pool.
    pub async fn sweep_old_entries(
        &self,
        max_age: std::time::Duration,
    ) -> Result<usize, AgentOSError> {
        let chrono_age = chrono::Duration::from_std(max_age)
            .map_err(|e| AgentOSError::StorageError(format!("Invalid max_age duration: {}", e)))?;
        let cutoff = (Utc::now() - chrono_age).to_rfc3339();
        let db = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock procedural db for sweep".to_string())
            })?;
            let tx = conn.transaction().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to begin sweep transaction: {}", e))
            })?;
            tx.execute(
                "DELETE FROM procedures_fts_content
                 WHERE proc_id IN (SELECT id FROM procedures WHERE updated_at < ?1)",
                params![cutoff],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to sweep old FTS rows: {}", e))
            })?;
            let deleted = tx
                .execute(
                    "DELETE FROM procedures WHERE updated_at < ?1",
                    params![cutoff],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to sweep old procedures: {}", e))
                })?;
            tx.commit().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to commit sweep transaction: {}", e))
            })?;
            Ok(deleted)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Sweep task panicked: {}", e)))?
    }

    fn row_to_procedure(row: &rusqlite::Row) -> rusqlite::Result<Procedure> {
        let id: String = row.get(0)?;
        let name: String = row.get(1)?;
        let description: String = row.get(2)?;
        let preconditions_json: String = row.get(3)?;
        let steps_json: String = row.get(4)?;
        let postconditions_json: String = row.get(5)?;
        let success_count: u32 = row.get(6)?;
        let failure_count: u32 = row.get(7)?;
        let source_episodes_json: String = row.get(8)?;
        let agent_id_str: Option<String> = row.get(9)?;
        let tags_json: String = row.get(10)?;
        let created_at: String = row.get(11)?;
        let updated_at: String = row.get(12)?;
        let (last_used_at, use_count, confidence, status) =
            crate::lifecycle::lifecycle_from_row(row, 13)?;

        Ok(Procedure {
            id,
            name,
            description,
            preconditions: serde_json::from_str(&preconditions_json).unwrap_or_default(),
            steps: serde_json::from_str(&steps_json).unwrap_or_default(),
            postconditions: serde_json::from_str(&postconditions_json).unwrap_or_default(),
            success_count,
            failure_count,
            source_episodes: serde_json::from_str(&source_episodes_json).unwrap_or_default(),
            agent_id: agent_id_str
                .and_then(|s| Uuid::parse_str(&s).ok())
                .map(AgentID::from_uuid),
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
                .unwrap_or_else(|_| chrono::Local::now().into())
                .with_timezone(&Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at)
                .unwrap_or_else(|_| chrono::Local::now().into())
                .with_timezone(&Utc),
            last_used_at,
            use_count,
            confidence,
            status,
        })
    }

    /// Count procedures, optionally scoped to an agent. Offloads to blocking thread pool.
    pub async fn count(&self, agent_id: Option<&AgentID>) -> Result<usize, AgentOSError> {
        let db = self.conn.clone();
        let agent_id_str = agent_id.map(|id| id.as_uuid().to_string());
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock procedural db for count".to_string())
            })?;
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM procedures WHERE (?1 IS NULL OR agent_id IS NULL OR agent_id = ?1)",
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

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm_a == 0.0 || norm_b == 0.0 {
            0.0
        } else {
            dot / (norm_a * norm_b)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryStatus;
    use tempfile::TempDir;

    fn make_test_procedure(name: &str, description: &str) -> Procedure {
        Procedure {
            id: String::new(),
            name: name.to_string(),
            description: description.to_string(),
            preconditions: vec!["repo clean".to_string()],
            steps: vec![
                ProcedureStep {
                    order: 0,
                    action: "run tests".to_string(),
                    tool: Some("shell-exec".to_string()),
                    expected_outcome: Some("all pass".to_string()),
                },
                ProcedureStep {
                    order: 1,
                    action: "deploy".to_string(),
                    tool: Some("shell-exec".to_string()),
                    expected_outcome: Some("service healthy".to_string()),
                },
            ],
            postconditions: vec!["deployment complete".to_string()],
            success_count: 0,
            failure_count: 0,
            source_episodes: vec!["ep-1".to_string()],
            agent_id: None,
            tags: vec!["ops".to_string()],
            created_at: chrono::DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now),
            updated_at: chrono::DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now),
            last_used_at: None,
            use_count: 0,
            confidence: crate::types::default_confidence(),
            status: MemoryStatus::Active,
        }
    }

    #[tokio::test]
    async fn test_store_and_get_procedure() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::noop());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();
        let proc = make_test_procedure("deploy", "Deploy application safely");

        let id = store.store(&proc).await.unwrap();
        let loaded = store.get(&id).await.unwrap().unwrap();
        assert_eq!(loaded.name, "deploy");
        assert_eq!(loaded.steps.len(), 2);
    }

    #[tokio::test]
    async fn test_search_procedure() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::noop());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();
        let deploy = make_test_procedure("deploy", "Deploy application safely");
        let backup = make_test_procedure("backup", "Create full data backup");
        store.store(&deploy).await.unwrap();
        store.store(&backup).await.unwrap();

        let results = store
            .search("application deployment", None, 5, 0.0)
            .await
            .unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_update_stats_and_delete() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::noop());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();
        let proc = make_test_procedure("deploy", "Deploy application safely");
        let id = store.store(&proc).await.unwrap();

        store.update_stats(&id, true).await.unwrap();
        let updated = store.get(&id).await.unwrap().unwrap();
        assert_eq!(updated.success_count, 1);

        store.delete(&id).await.unwrap();
        assert!(store.get(&id).await.unwrap().is_none());
    }

    /// Build a procedural DB with the pre-lifecycle schema and one legacy row.
    fn seed_legacy_db(dir: &std::path::Path) {
        let conn = Connection::open(dir.join("procedural_memory.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE procedures (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                description     TEXT NOT NULL,
                preconditions   TEXT NOT NULL,
                steps           TEXT NOT NULL,
                postconditions  TEXT NOT NULL,
                success_count   INTEGER NOT NULL DEFAULT 0,
                failure_count   INTEGER NOT NULL DEFAULT 0,
                source_episodes TEXT NOT NULL,
                agent_id        TEXT,
                tags            TEXT NOT NULL,
                embedding       BLOB NOT NULL,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );",
        )
        .unwrap();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO procedures (id, name, description, preconditions, steps, postconditions,
                success_count, failure_count, source_episodes, agent_id, tags, embedding,
                created_at, updated_at)
             VALUES ('legacy-1', 'legacy-proc', 'old row', '[]', '[]', '[]',
                3, 1, '[]', NULL, '[]', ?1, ?2, ?2)",
            params![vec![0u8; 384 * 4], now],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn migration_adds_lifecycle_columns_to_legacy_db() {
        let dir = TempDir::new().unwrap();
        seed_legacy_db(dir.path());

        let embedder = Arc::new(Embedder::noop());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let legacy = store.get("legacy-1").await.unwrap().unwrap();
        assert_eq!(legacy.name, "legacy-proc");
        assert_eq!(legacy.success_count, 3);
        assert_eq!(legacy.use_count, 0);
        assert!(legacy.last_used_at.is_none());
        assert!((legacy.confidence - 0.6).abs() < f32::EPSILON);
        assert_eq!(legacy.status, MemoryStatus::Active);
    }

    #[tokio::test]
    async fn migration_is_idempotent() {
        let dir = TempDir::new().unwrap();
        seed_legacy_db(dir.path());

        let embedder = Arc::new(Embedder::noop());
        drop(ProceduralStore::open_with_embedder(dir.path(), embedder.clone()).unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();
        assert!(store.get("legacy-1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn touch_updates_recency_and_count() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::noop());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();
        let proc = make_test_procedure("deploy", "Deploy application safely");
        let id = store.store(&proc).await.unwrap();

        assert_eq!(store.touch(std::slice::from_ref(&id)).await.unwrap(), 1);
        assert_eq!(store.touch(std::slice::from_ref(&id)).await.unwrap(), 1);
        let loaded = store.get(&id).await.unwrap().unwrap();
        assert_eq!(loaded.use_count, 2);
        assert!(loaded.last_used_at.is_some());

        // Empty and unknown ids are harmless no-ops.
        assert_eq!(store.touch(&[]).await.unwrap(), 0);
        assert_eq!(store.touch(&["missing".to_string()]).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn update_stats_recomputes_laplace_confidence() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::noop());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();
        let proc = make_test_procedure("deploy", "Deploy application safely");
        let id = store.store(&proc).await.unwrap();

        // 1 success, 0 failures → (1+1)/(1+0+2) = 2/3
        store.update_stats(&id, true).await.unwrap();
        let p = store.get(&id).await.unwrap().unwrap();
        assert!(
            (p.confidence - 2.0 / 3.0).abs() < 1e-6,
            "got {}",
            p.confidence
        );

        // 1 success, 1 failure → (1+1)/(1+1+2) = 0.5
        store.update_stats(&id, false).await.unwrap();
        let p = store.get(&id).await.unwrap().unwrap();
        assert!((p.confidence - 0.5).abs() < 1e-6, "got {}", p.confidence);
    }

    #[tokio::test]
    async fn lifecycle_fields_roundtrip() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::noop());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let mut proc = make_test_procedure("deploy", "Deploy application safely");
        proc.last_used_at = Some(Utc::now());
        proc.use_count = 7;
        proc.confidence = 0.42;
        proc.status = MemoryStatus::Stale;

        let id = store.store(&proc).await.unwrap();
        let loaded = store.get(&id).await.unwrap().unwrap();
        assert_eq!(loaded.use_count, 7);
        assert!((loaded.confidence - 0.42).abs() < 1e-6);
        assert_eq!(loaded.status, MemoryStatus::Stale);
        assert!(loaded.last_used_at.is_some());
    }
}
