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

    /// Write a memory entry - automatically chunks and computes embeddings via `fastembed`
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

        let chunks = Embedder::chunk_text(content, 2000, 200);
        let chunks_refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();

        let embeddings = self.embedder.embed(&chunks_refs).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to compute embedding: {}", e))
        })?;

        let conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock semantic db for writing".to_string())
        })?;

        conn.execute("BEGIN TRANSACTION", [])
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        if let Err(e) = conn.execute(
            "INSERT INTO semantic_memory (id, agent_id, key, content, created_at, updated_at, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![mem_id, agent_id_str, key, content, now, now, tags_str],
        ) {
            conn.execute("ROLLBACK TRANSACTION", []).ok();
            return Err(AgentOSError::StorageError(format!(
                "Insert memory failed: {}",
                e
            )));
        }

        for (i, (chunk_text, embedding_vec)) in chunks.iter().zip(embeddings).enumerate() {
            let chunk_id = Uuid::new_v4().to_string();
            let mut blob = Vec::with_capacity(embedding_vec.len() * 4);
            for val in embedding_vec {
                blob.extend_from_slice(&val.to_le_bytes());
            }

            if let Err(e) = conn.execute(
                "INSERT INTO semantic_chunks (id, memory_id, chunk_index, content, embedding)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![chunk_id, mem_id, i, chunk_text, blob],
            ) {
                conn.execute("ROLLBACK TRANSACTION", []).ok();
                return Err(AgentOSError::StorageError(format!(
                    "Insert chunk failed: {}",
                    e
                )));
            }
        }

        conn.execute("COMMIT TRANSACTION", [])
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        Ok(mem_id)
    }

    /// Semantic search using cosine similarity over chunk embeddings.
    /// Returns the best-matching chunk per memory entry.
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

        let query_embedding = self
            .embedder
            .embed(&[query])
            .map_err(|e| AgentOSError::StorageError(format!("Query embed error: {}", e)))?
            .pop()
            .ok_or_else(|| {
                AgentOSError::StorageError("Query embedding returned empty result".to_string())
            })?;

        if query_embedding.len() != self.dimension {
            return Err(AgentOSError::StorageError(format!(
                "Query embedding dimension mismatch: expected {}, got {}",
                self.dimension,
                query_embedding.len()
            )));
        }

        let conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock semantic db for search".to_string())
        })?;

        let agent_id_str = agent_id.map(|id| id.as_uuid().to_string());
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.agent_id, m.key, m.content, m.created_at, m.updated_at, m.tags,
                        c.id, c.chunk_index, c.content, c.embedding
                 FROM semantic_chunks c
                 JOIN semantic_memory m ON c.memory_id = m.id
                 WHERE (?1 IS NULL OR m.agent_id IS NULL OR m.agent_id = ?1)",
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let chunk_rows = stmt
            .query_map(params![agent_id_str], |row| {
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
                        tags: serde_json::from_str(&tags_str.unwrap_or_default())
                            .unwrap_or_default(),
                    },
                    MemoryChunk {
                        id: c_id,
                        memory_id: m_id,
                        chunk_index: c_index,
                        content: c_content,
                    },
                    embedding,
                ))
            })
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let mut best_by_memory: HashMap<String, RecallResult> = HashMap::new();
        for row in chunk_rows {
            let (entry, chunk, embedding) =
                row.map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            if embedding.len() != self.dimension {
                continue;
            }

            let score = Self::cosine_similarity(&query_embedding, &embedding);
            if score < min_score {
                continue;
            }

            let candidate = RecallResult {
                entry: entry.clone(),
                chunk,
                semantic_score: score,
                fts_score: 0.0,
                rrf_score: score,
            };

            match best_by_memory.get_mut(&entry.id) {
                Some(existing) if score > existing.semantic_score => *existing = candidate,
                None => {
                    best_by_memory.insert(entry.id, candidate);
                }
                _ => {}
            }
        }

        let mut results: Vec<RecallResult> = best_by_memory.into_values().collect();
        results.sort_by(|a, b| {
            b.semantic_score
                .partial_cmp(&a.semantic_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);

        Ok(results)
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

    /// Exact key lookup.
    pub fn get_by_key(&self, key: &str) -> Result<Option<MemoryEntry>, AgentOSError> {
        let conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock semantic db for get_by_key".to_string())
        })?;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, key, content, created_at, updated_at, tags
                 FROM semantic_memory WHERE key = ?1",
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![key], |row| {
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
                    tags: serde_json::from_str(&tags_str.unwrap_or_default()).unwrap_or_default(),
                })
            })
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        match rows.next() {
            Some(Ok(entry)) => Ok(Some(entry)),
            Some(Err(e)) => Err(AgentOSError::StorageError(e.to_string())),
            None => Ok(None),
        }
    }

    /// Delete a memory entry and its chunks by ID (transactional).
    pub fn delete(&self, id: &str) -> Result<(), AgentOSError> {
        let mut conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock semantic db for delete".to_string())
        })?;

        let tx = conn.transaction().map_err(|e| {
            AgentOSError::StorageError(format!("Failed to start transaction: {}", e))
        })?;

        tx.execute(
            "DELETE FROM semantic_chunks WHERE memory_id = ?1",
            params![id],
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to delete chunks: {}", e)))?;

        let deleted = tx
            .execute("DELETE FROM semantic_memory WHERE id = ?1", params![id])
            .map_err(|e| AgentOSError::StorageError(format!("Failed to delete memory: {}", e)))?;

        if deleted == 0 {
            return Err(AgentOSError::StorageError(format!(
                "Memory entry '{}' not found",
                id
            )));
        }

        tx.commit()
            .map_err(|e| AgentOSError::StorageError(format!("Failed to commit delete: {}", e)))?;

        Ok(())
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
