use crate::embedder::Embedder;
use crate::types::{MemoryChunk, MemoryEntry, RecallResult};
use agentos_types::{AgentID, AgentOSError};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

const EXPECTED_EMBEDDING_DIMENSION: usize = 384;

pub struct SemanticStore {
    conn: Arc<std::sync::Mutex<Connection>>,
    embedder: Arc<Embedder>,
    dimension: usize,
}

impl SemanticStore {
    /// Open semantic memory using the default model cache dir under `{data_dir}/models`.
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError> {
        Self::open_with_cache_dir(data_dir, &data_dir.join("models"))
    }

    /// Open semantic memory with an explicit embedding model cache directory.
    pub fn open_with_cache_dir(
        data_dir: &Path,
        model_cache_dir: &Path,
    ) -> Result<Self, AgentOSError> {
        let embedder = Arc::new(Embedder::with_cache_dir(model_cache_dir).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to initialize embedder: {}", e))
        })?);
        Self::open_with_embedder(data_dir, embedder)
    }

    /// Open semantic memory with a caller-provided embedder.
    pub fn open_with_embedder(
        data_dir: &Path,
        embedder: Arc<Embedder>,
    ) -> Result<Self, AgentOSError> {
        let db_path = data_dir.join("semantic_memory.db");
        let conn = Connection::open(&db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open semantic memory DB: {}", e))
        })?;

        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS semantic_memory (
                id          TEXT PRIMARY KEY,
                agent_id    TEXT,
                key         TEXT NOT NULL,
                content     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                tags        TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_sem_agent ON semantic_memory(agent_id);
            CREATE INDEX IF NOT EXISTS idx_sem_key ON semantic_memory(key);

            CREATE TABLE IF NOT EXISTS semantic_chunks (
                id          TEXT PRIMARY KEY,
                memory_id   TEXT NOT NULL,
                chunk_index INTEGER NOT NULL,
                content     TEXT NOT NULL,
                embedding   BLOB NOT NULL,
                FOREIGN KEY(memory_id) REFERENCES semantic_memory(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_chunk_mem ON semantic_chunks(memory_id);

            CREATE VIRTUAL TABLE IF NOT EXISTS semantic_fts USING fts5(
                content,
                content='semantic_chunks',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS sem_ai AFTER INSERT ON semantic_chunks BEGIN
              INSERT INTO semantic_fts(rowid, content) VALUES (new.rowid, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS sem_ad AFTER DELETE ON semantic_chunks BEGIN
              INSERT INTO semantic_fts(semantic_fts, rowid, content) VALUES('delete', old.rowid, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS sem_au AFTER UPDATE ON semantic_chunks BEGIN
              INSERT INTO semantic_fts(semantic_fts, rowid, content) VALUES('delete', old.rowid, old.content);
              INSERT INTO semantic_fts(rowid, content) VALUES (new.rowid, new.content);
            END;
        ",
        )
        .map_err(|e| {
            AgentOSError::StorageError(format!("Failed to init semantic memory tables: {}", e))
        })?;

        let probe = embedder
            .embed(&["semantic-memory-dimension-probe"])
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
            conn: Arc::new(std::sync::Mutex::new(conn)),
            embedder,
            dimension,
        })
    }

    /// Write a memory entry - automatically chunks and computes embeddings via `fastembed`.
    /// The SQLite write is offloaded to a blocking thread pool via `spawn_blocking` so it
    /// does not block the async runtime under concurrent agent load.
    pub async fn write(
        &self,
        key: &str,
        content: &str,
        agent_id: Option<&AgentID>,
        tags: &[&str],
    ) -> Result<String, AgentOSError> {
        let mem_id = Uuid::new_v4().to_string();
        let agent_id_str = agent_id.map(|id| id.as_uuid().to_string());
        let tags_str = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let now = Utc::now().to_rfc3339();
        let key_owned = key.to_owned();
        let content_owned = content.to_owned();
        let embedder = self.embedder.clone();
        let db = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            // Embedding is CPU-intensive; run it here on the blocking thread pool
            // so async worker threads are not blocked by ONNX model inference.
            let chunks = Embedder::chunk_text(&content_owned, 2000, 200);
            let chunks_refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
            let embeddings = embedder.embed(&chunks_refs).map_err(|e| {
                AgentOSError::StorageError(format!("Failed to compute embedding: {}", e))
            })?;
            let mut conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock semantic db for writing".to_string())
            })?;

            // Use rusqlite's Transaction so the DB is automatically rolled back on any
            // early return (including panics), rather than relying on manual ROLLBACK calls.
            let tx = conn.transaction().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to begin transaction: {}", e))
            })?;

            tx.execute(
                "INSERT INTO semantic_memory (id, agent_id, key, content, created_at, updated_at, tags)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![mem_id, agent_id_str, key_owned, content_owned, now, now, tags_str],
            )
            .map_err(|e| AgentOSError::StorageError(format!("Insert memory failed: {}", e)))?;

            for (i, (chunk_text, embedding_vec)) in chunks.iter().zip(embeddings).enumerate() {
                let chunk_id = Uuid::new_v4().to_string();
                let mut blob = Vec::with_capacity(embedding_vec.len() * 4);
                for val in embedding_vec {
                    blob.extend_from_slice(&val.to_le_bytes());
                }

                tx.execute(
                    "INSERT INTO semantic_chunks (id, memory_id, chunk_index, content, embedding)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![chunk_id, &mem_id, i, chunk_text, blob],
                )
                .map_err(|e| AgentOSError::StorageError(format!("Insert chunk failed: {}", e)))?;
            }

            tx.commit().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to commit transaction: {}", e))
            })?;

            Ok(mem_id)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Write task panicked: {}", e)))?
    }

    /// Maximum candidate chunks to load for cosine similarity when FTS5 pre-filter is used.
    const FTS_CANDIDATE_LIMIT: usize = 200;
    /// Fallback limit when FTS5 finds no matches — load most recent chunks instead of all.
    const RECENCY_FALLBACK_LIMIT: usize = 500;
    /// Reciprocal Rank Fusion constant (k=60 is standard).
    const RRF_K: f32 = 60.0;

    /// Hybrid semantic search using FTS5 pre-filter + cosine similarity.
    ///
    /// Phase 1: Use FTS5 full-text search to find top candidate chunks by text relevance.
    /// Phase 2: Load only those chunk embeddings and compute cosine similarity.
    /// Phase 3: Combine FTS rank + vector score via Reciprocal Rank Fusion (RRF).
    ///
    /// If FTS5 finds no matches (e.g., query uses synonyms not in the text), falls back
    /// to loading the most recent chunks (bounded to RECENCY_FALLBACK_LIMIT).
    ///
    /// The SQLite operations are offloaded via `spawn_blocking`.
    pub async fn search(
        &self,
        query: &str,
        agent_id: Option<&AgentID>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<RecallResult>, AgentOSError> {
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
                .pop()
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
                AgentOSError::StorageError("Failed to lock semantic db for search".to_string())
            })?;

            // Phase 1: FTS5 pre-filter — find candidate chunk rowids by text relevance
            // Sanitize query to prevent FTS5 operator injection (*, OR, NOT, NEAR, etc.)
            let sanitized_query = format!("\"{}\"", query_owned.replace('"', "\"\""));
            let fts_ranks: HashMap<i64, f32> = {
                let mut fts_map = HashMap::new();
                // FTS5 MATCH can fail on certain query syntax; fall back gracefully
                if let Ok(mut fts_stmt) = conn.prepare(
                    "SELECT rowid, rank FROM semantic_fts WHERE semantic_fts MATCH ?1 ORDER BY rank LIMIT ?2",
                ) {
                    if let Ok(rows) = fts_stmt.query_map(
                        params![sanitized_query, SemanticStore::FTS_CANDIDATE_LIMIT as i64],
                        |row| {
                            let rowid: i64 = row.get(0)?;
                            let rank: f64 = row.get(1)?;
                            Ok((rowid, rank as f32))
                        },
                    ) {
                        for row in rows.flatten() {
                            fts_map.insert(row.0, row.1);
                        }
                    }
                }
                fts_map
            };

            let use_fts = !fts_ranks.is_empty();

            // Row mapper closure shared by both query paths.
            let map_row = |row: &rusqlite::Row<'_>| {
                let m_id: String = row.get(0)?;
                let a_id: Option<String> = row.get(1)?;
                let key: String = row.get(2)?;
                let content: String = row.get(3)?;
                let created_at: String = row.get(4)?;
                let updated_at: String = row.get(5)?;
                let tags_str: Option<String> = row.get(6)?;

                let c_id: String = row.get(7)?;
                let c_index: usize = row.get(8)?;
                let c_content: String = row.get(9)?;
                let blob: Vec<u8> = row.get(10)?;
                let rowid: i64 = row.get(11)?;

                let mut embedding = Vec::with_capacity(blob.len() / 4);
                for bytes in blob.chunks_exact(4) {
                    embedding.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                }

                let parsed_agent_id =
                    a_id.map(|s| AgentID::from_uuid(Uuid::parse_str(&s).unwrap_or_default()));
                let parsed_created = chrono::DateTime::parse_from_rfc3339(&created_at)
                    .unwrap_or_else(|_| chrono::Local::now().into())
                    .with_timezone(&Utc);
                let parsed_updated = chrono::DateTime::parse_from_rfc3339(&updated_at)
                    .unwrap_or_else(|_| chrono::Local::now().into())
                    .with_timezone(&Utc);

                Ok((
                    MemoryEntry {
                        id: m_id.clone(),
                        agent_id: parsed_agent_id,
                        key,
                        full_content: content,
                        created_at: parsed_created,
                        updated_at: parsed_updated,
                        tags: serde_json::from_str(&tags_str.unwrap_or_default()).unwrap_or_default(),
                    },
                    MemoryChunk {
                        id: c_id,
                        memory_id: m_id,
                        chunk_index: c_index,
                        content: c_content,
                    },
                    embedding,
                    rowid,
                ))
            };

            // Phase 2: Load chunk embeddings — only for FTS candidates, or recent chunks as fallback.
            //
            // For the FTS path: build a parameterized IN clause using ?N placeholders so that
            // rowid values (which come from a prior SQLite FTS5 query, not user input) follow
            // the same parameterized-query convention as the rest of the codebase.
            let chunk_rows: Vec<(MemoryEntry, MemoryChunk, Vec<f32>, i64)> = if use_fts {
                let rowids: Vec<i64> = fts_ranks.keys().copied().collect();
                // agent_id_str is bound as ?1; rowids are bound as ?2..?N
                let placeholders = (2..=rowids.len() + 1)
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT m.id, m.agent_id, m.key, m.content, m.created_at, m.updated_at, m.tags,
                            c.id, c.chunk_index, c.content, c.embedding, c.rowid
                     FROM semantic_chunks c
                     JOIN semantic_memory m ON c.memory_id = m.id
                     WHERE c.rowid IN ({})
                       AND (?1 IS NULL OR m.agent_id IS NULL OR m.agent_id = ?1)",
                    placeholders
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
                let rows: Result<_, rusqlite::Error> = stmt
                    .query_map(rusqlite::params_from_iter(bound.iter()), map_row)
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?
                    .collect();
                rows.map_err(|e| AgentOSError::StorageError(e.to_string()))?
            } else {
                // Fallback: load most recent chunks (bounded); LIMIT is a constant, not user input.
                let sql = format!(
                    "SELECT m.id, m.agent_id, m.key, m.content, m.created_at, m.updated_at, m.tags,
                            c.id, c.chunk_index, c.content, c.embedding, c.rowid
                     FROM semantic_chunks c
                     JOIN semantic_memory m ON c.memory_id = m.id
                     WHERE (?1 IS NULL OR m.agent_id IS NULL OR m.agent_id = ?1)
                     ORDER BY c.rowid DESC
                     LIMIT {}",
                    SemanticStore::RECENCY_FALLBACK_LIMIT
                );
                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
                let rows: Result<_, rusqlite::Error> = stmt
                    .query_map(params![agent_id_str], map_row)
                    .map_err(|e| AgentOSError::StorageError(e.to_string()))?
                    .collect();
                rows.map_err(|e| AgentOSError::StorageError(e.to_string()))?
            };

            // Phase 3: Score chunks and apply RRF fusion
            let mut best_by_memory: HashMap<String, RecallResult> = HashMap::new();
            for (entry, chunk, embedding, rowid) in chunk_rows {
                if embedding.len() != dimension {
                    continue;
                }

                let semantic_score = SemanticStore::cosine_similarity(&query_embedding, &embedding);
                if semantic_score < min_score {
                    continue;
                }

                // FTS score (negative rank from FTS5 — more negative = better match)
                let fts_score = fts_ranks.get(&rowid).map(|r| -r).unwrap_or(0.0);

                // RRF: combine semantic rank and FTS rank
                let rrf_score = if use_fts && fts_score > 0.0 {
                    let fts_normalized = fts_score / (fts_score + SemanticStore::RRF_K);
                    0.7 * semantic_score + 0.3 * fts_normalized
                } else {
                    semantic_score
                };

                let candidate = RecallResult {
                    entry: entry.clone(),
                    chunk,
                    semantic_score,
                    fts_score,
                    rrf_score,
                };

                match best_by_memory.get_mut(&entry.id) {
                    Some(existing) if rrf_score > existing.rrf_score => *existing = candidate,
                    None => {
                        best_by_memory.insert(entry.id, candidate);
                    }
                    _ => {}
                }
            }

            let mut results: Vec<RecallResult> = best_by_memory.into_values().collect();
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

    /// Compatibility wrapper for existing callers.
    pub async fn search_hybrid(
        &self,
        query: &str,
        agent_id: Option<&AgentID>,
        top_k: usize,
    ) -> Result<Vec<RecallResult>, AgentOSError> {
        self.search(query, agent_id, top_k, 0.0).await
    }

    /// Exact key lookup. Offloads SQLite read to the blocking thread pool.
    pub async fn get_by_key(&self, key: &str) -> Result<Option<MemoryEntry>, AgentOSError> {
        let db = self.conn.clone();
        let key_owned = key.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock semantic db for get_by_key".to_string())
            })?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, agent_id, key, content, created_at, updated_at, tags
                     FROM semantic_memory WHERE key = ?1",
                )
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            let mut rows = stmt
                .query_map(params![key_owned], |row| {
                    let id: String = row.get(0)?;
                    let agent_id_str: Option<String> = row.get(1)?;
                    let key: String = row.get(2)?;
                    let content: String = row.get(3)?;
                    let created_at: String = row.get(4)?;
                    let updated_at: String = row.get(5)?;
                    let tags_str: Option<String> = row.get(6)?;

                    Ok(MemoryEntry {
                        id,
                        agent_id: agent_id_str
                            .map(|s| AgentID::from_uuid(Uuid::parse_str(&s).unwrap_or_default())),
                        key,
                        full_content: content,
                        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
                            .unwrap_or_else(|_| chrono::Local::now().into())
                            .with_timezone(&Utc),
                        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at)
                            .unwrap_or_else(|_| chrono::Local::now().into())
                            .with_timezone(&Utc),
                        tags: serde_json::from_str(&tags_str.unwrap_or_default())
                            .unwrap_or_default(),
                    })
                })
                .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            match rows.next() {
                Some(Ok(entry)) => Ok(Some(entry)),
                Some(Err(e)) => Err(AgentOSError::StorageError(e.to_string())),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("get_by_key task panicked: {}", e)))?
    }

    /// Delete a memory entry and its chunks by ID (transactional).
    /// Offloads SQLite work to the blocking thread pool.
    pub async fn delete(&self, id: &str) -> Result<(), AgentOSError> {
        let db = self.conn.clone();
        let id_owned = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock semantic db for delete".to_string())
            })?;

            let tx = conn.transaction().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to start transaction: {}", e))
            })?;

            tx.execute(
                "DELETE FROM semantic_chunks WHERE memory_id = ?1",
                params![id_owned],
            )
            .map_err(|e| AgentOSError::StorageError(format!("Failed to delete chunks: {}", e)))?;

            let deleted = tx
                .execute(
                    "DELETE FROM semantic_memory WHERE id = ?1",
                    params![id_owned],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to delete memory: {}", e))
                })?;

            if deleted == 0 {
                return Err(AgentOSError::StorageError(format!(
                    "Memory entry '{}' not found",
                    id_owned
                )));
            }

            tx.commit().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to commit delete: {}", e))
            })?;

            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Delete task panicked: {}", e)))?
    }

    /// Delete memory entries older than `max_age` and return the number deleted.
    ///
    /// This is the archival sweep for Tier 3 persistent memory (Spec §11).
    /// Entries whose `updated_at` timestamp is older than `max_age` ago are removed.
    /// Offloads SQLite work to the blocking thread pool.
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
                AgentOSError::StorageError("Failed to lock semantic db for sweep".to_string())
            })?;

            let tx = conn.transaction().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to begin sweep transaction: {}", e))
            })?;

            // Delete chunks first (FK cascade should handle this, but be explicit)
            tx.execute(
                "DELETE FROM semantic_chunks WHERE memory_id IN (SELECT id FROM semantic_memory WHERE updated_at < ?1)",
                params![cutoff],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to sweep old chunks: {}", e))
            })?;

            let deleted = tx
                .execute(
                    "DELETE FROM semantic_memory WHERE updated_at < ?1",
                    params![cutoff],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to sweep old entries: {}", e))
                })?;

            tx.commit().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to commit sweep transaction: {}", e))
            })?;

            Ok(deleted)
        })
        .await
        .map_err(|e| AgentOSError::StorageError(format!("Sweep task panicked: {}", e)))?
    }

    /// Export all memory entries as newline-delimited JSON (JSONL) to the given writer.
    ///
    /// Each line is a JSON object with fields: id, agent_id, key, content, created_at,
    /// updated_at, tags. This enables Tier 3 archival export (Spec §11).
    ///
    /// Note: This is a synchronous batch utility. Do not call from hot-path async code.
    pub fn export_jsonl<W: std::io::Write>(&self, writer: &mut W) -> Result<usize, AgentOSError> {
        let conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock semantic db for export".to_string())
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, key, content, created_at, updated_at, tags
                 FROM semantic_memory ORDER BY created_at ASC",
            )
            .map_err(|e| AgentOSError::StorageError(format!("Export prepare error: {}", e)))?;

        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let agent_id: Option<String> = row.get(1)?;
                let key: String = row.get(2)?;
                let content: String = row.get(3)?;
                let created_at: String = row.get(4)?;
                let updated_at: String = row.get(5)?;
                let tags: Option<String> = row.get(6)?;
                Ok(serde_json::json!({
                    "id": id,
                    "agent_id": agent_id,
                    "key": key,
                    "content": content,
                    "created_at": created_at,
                    "updated_at": updated_at,
                    "tags": tags,
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

    /// Import memory entries from newline-delimited JSON (JSONL).
    ///
    /// Each line must be a JSON object with fields: key, content, and optionally
    /// agent_id, tags. The `id`, `created_at`, and `updated_at` fields from the
    /// JSONL are used if present; otherwise new values are generated.
    ///
    /// Returns the number of entries imported.
    pub async fn import_jsonl<R: std::io::BufRead>(
        &self,
        reader: R,
    ) -> Result<usize, AgentOSError> {
        let mut count = 0;
        for line in reader.lines() {
            let line = line.map_err(|e| {
                AgentOSError::StorageError(format!("Failed to read JSONL line: {}", e))
            })?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let obj: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                AgentOSError::StorageError(format!("Invalid JSON on line {}: {}", count + 1, e))
            })?;

            let key = obj["key"].as_str().ok_or_else(|| {
                AgentOSError::StorageError(format!("Missing 'key' field on line {}", count + 1))
            })?;
            let content = obj["content"].as_str().ok_or_else(|| {
                AgentOSError::StorageError(format!("Missing 'content' field on line {}", count + 1))
            })?;

            let agent_id = obj["agent_id"]
                .as_str()
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .map(AgentID::from_uuid);

            let tags: Vec<String> = obj["tags"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .or_else(|| {
                    obj["tags"].as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                })
                .unwrap_or_default();

            let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
            self.write(key, content, agent_id.as_ref(), &tag_refs)
                .await?;
            count += 1;
        }
        Ok(count)
    }

    /// Count entries, optionally scoped to an agent. Offloads to the blocking thread pool.
    pub async fn count(&self, agent_id: Option<&AgentID>) -> Result<usize, AgentOSError> {
        let db = self.conn.clone();
        let agent_id_str = agent_id.map(|id| id.as_uuid().to_string());
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().map_err(|_| {
                AgentOSError::StorageError("Failed to lock semantic db for count".to_string())
            })?;
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM semantic_memory WHERE (?1 IS NULL OR agent_id IS NULL OR agent_id = ?1)",
                    params![agent_id_str],
                    |row| row.get(0),
                )
                .map_err(|e| AgentOSError::StorageError(format!("Count query failed: {}", e)))?;
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
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_semantic_search_finds_similar_content() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = SemanticStore::open_with_embedder(dir.path(), embedder).unwrap();

        store
            .write(
                "deployment",
                "We deploy our app using Docker containers.",
                None,
                &[],
            )
            .await
            .unwrap();
        store
            .write("weather", "Today it is sunny and warm.", None, &[])
            .await
            .unwrap();

        let results = store
            .search("Kubernetes container deployment", None, 3, 0.0)
            .await
            .unwrap();

        let deployment_score = results
            .iter()
            .find(|r| r.entry.key == "deployment")
            .map(|r| r.semantic_score)
            .unwrap_or(0.0);
        let weather_score = results
            .iter()
            .find(|r| r.entry.key == "weather")
            .map(|r| r.semantic_score)
            .unwrap_or(0.0);

        assert!(
            deployment_score > weather_score,
            "Expected 'deployment' to rank higher than 'weather'"
        );
    }
}
