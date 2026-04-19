---
title: "Phase 4: Procedural Memory Tier"
tags:
  - plan
  - memory
  - procedural
  - v3
date: 2026-03-12
status: complete
effort: 2d
priority: high
---

# Phase 4: Procedural Memory Tier

> Add a third persistent memory tier for skills, SOPs, and learned procedures — enabling agents to accumulate "how-to" knowledge from repeated task patterns.

---

## Why This Phase

Episodic memory answers "what happened when I tried X?". Procedural memory answers "what is the best way to do X?". Without this tier, agents relearn the same procedures every session. The Voyager system showed that code-as-skills with semantic retrieval dramatically improves task completion. Our version stores structured records instead of executable code, fitting Rust's type system. Research from Letta/MemGPT confirms that accumulated procedural knowledge reduces per-task token costs by 40–60% for recurring workflows.

---

## Current State

- `agentos-memory` has `SemanticStore` (facts) and `EpisodicStore` (events)
- Both use SQLite + FTS5 + vector embeddings with hybrid RRF search
- `Embedder` handles AllMiniLML6V2 (384-dim) embeddings
- `SemanticStore` stores chunks with `Mutex<Connection>` for thread safety, `open_with_embedder()` for DI
- No procedural/skill storage exists
- `AgentOSError::StorageError(String)` is the standard storage error variant
- All SQL uses `params![]` parameterized queries — no string interpolation

## Target State

- `ProceduralStore` in `agentos-memory` — same `Mutex<Connection>` + FTS5 + cosine + RRF pattern as `SemanticStore`
- `Procedure` and `ProcedureStep` types in `types.rs` with full serde derives
- `ProcedureSearchResult` for search return values with score breakdown
- Hybrid search (FTS + cosine) matching the existing RRF fusion weighting (70% semantic / 30% FTS)
- CRUD: `store()`, `search()`, `get()`, `update_stats()`, `delete()`, `list_by_agent()`
- Queryable by `ContextCompiler` (Phase 3) to inject relevant procedures into the Knowledge category
- Inline `#[cfg(test)]` module with `tempfile::TempDir` isolation

---

## Subtasks

### 4.1 Define `Procedure`, `ProcedureStep`, and `ProcedureSearchResult` types

**File:** `crates/agentos-memory/src/types.rs`

Append after the existing `RecallResult` struct (line 95):

```rust
/// A single step in a stored procedure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureStep {
    /// Execution order (0-indexed).
    pub order: usize,
    /// Human-readable action description.
    pub action: String,
    /// Tool name to invoke for this step (if applicable).
    pub tool: Option<String>,
    /// What success looks like for this step.
    pub expected_outcome: Option<String>,
}

/// A stored procedure representing a learned skill or SOP.
///
/// Procedures are distilled from repeated episodic patterns (Phase 7 consolidation)
/// or created explicitly by agents (Phase 8 memory self-management).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Procedure {
    /// UUID primary key.
    pub id: String,
    /// Short descriptive name, e.g. "deploy-to-production".
    pub name: String,
    /// What this procedure accomplishes.
    pub description: String,
    /// Conditions that must hold before execution.
    pub preconditions: Vec<String>,
    /// Ordered steps.
    pub steps: Vec<ProcedureStep>,
    /// Expected outcomes after successful execution.
    pub postconditions: Vec<String>,
    /// Times this procedure led to a successful outcome.
    pub success_count: u32,
    /// Times this procedure led to a failure.
    pub failure_count: u32,
    /// Episodic entry IDs this procedure was distilled from.
    pub source_episodes: Vec<String>,
    /// Owning agent (None = globally available).
    pub agent_id: Option<AgentID>,
    /// Free-form tags for categorization.
    pub tags: Vec<String>,
    /// When this procedure was first created.
    pub created_at: DateTime<Utc>,
    /// When this procedure was last modified.
    pub updated_at: DateTime<Utc>,
}

/// Result of a hybrid procedural search with score breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureSearchResult {
    pub procedure: Procedure,
    /// Cosine similarity between query embedding and procedure embedding.
    pub semantic_score: f32,
    /// BM25 / FTS5 rank score (negated — higher is better).
    pub fts_score: f32,
    /// Reciprocal Rank Fusion score (70% semantic + 30% FTS).
    pub rrf_score: f32,
}
```

**Required import additions** at the top of `types.rs` — `AgentID` is already imported via the existing `use agentos_types::{AgentID, TaskID, TraceID};` and `DateTime<Utc>` is already imported via the existing `use chrono::{DateTime, Utc};`.

No new imports needed.

---

### 4.2 Create `ProceduralStore`

**File:** `crates/agentos-memory/src/procedural.rs` (new file)

This is the complete implementation. It mirrors `SemanticStore` conventions: `Mutex<Connection>`, `Arc<Embedder>`, `open()` / `open_with_embedder()` constructors, `AgentOSError::StorageError` for all errors, parameterized SQL throughout.

```rust
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
                embedding       BLOB,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_proc_agent ON procedures(agent_id);
            CREATE INDEX IF NOT EXISTS idx_proc_name ON procedures(name);
            CREATE INDEX IF NOT EXISTS idx_proc_updated ON procedures(updated_at);

            CREATE VIRTUAL TABLE IF NOT EXISTS procedures_fts USING fts5(
                name, description, steps_text,
                content='procedures_fts_content',
                content_rowid='rowid'
            );

            CREATE TABLE IF NOT EXISTS procedures_fts_content (
                rowid       INTEGER PRIMARY KEY AUTOINCREMENT,
                proc_id     TEXT NOT NULL,
                name        TEXT NOT NULL,
                description TEXT NOT NULL,
                steps_text  TEXT NOT NULL,
                UNIQUE(proc_id)
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
            AgentOSError::StorageError(format!(
                "Failed to init procedural memory tables: {}",
                e
            ))
        })?;

        // Probe embedding dimension
        let probe = embedder
            .embed(&["procedural-memory-dimension-probe"])
            .map_err(|e| {
                AgentOSError::StorageError(format!("Embedding dimension probe failed: {}", e))
            })?;
        let dimension = probe.first().map(|v| v.len()).ok_or_else(|| {
            AgentOSError::StorageError(
                "Embedding model returned empty probe result".to_string(),
            )
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

    /// Build the text used for embedding a procedure.
    ///
    /// Format: "name: description. Steps: step1; step2; step3"
    /// This gives the embedding model a dense summary of the procedure's purpose and actions.
    fn build_embedding_text(procedure: &Procedure) -> String {
        let steps_summary: String = procedure
            .steps
            .iter()
            .map(|s| s.action.as_str())
            .collect::<Vec<&str>>()
            .join("; ");
        format!(
            "{}: {}. Steps: {}",
            procedure.name, procedure.description, steps_summary
        )
    }

    /// Flatten steps into a single searchable text for FTS5.
    fn build_steps_text(steps: &[ProcedureStep]) -> String {
        steps
            .iter()
            .map(|s| {
                let mut text = s.action.clone();
                if let Some(ref tool) = s.tool {
                    text.push_str(" [tool: ");
                    text.push_str(tool);
                    text.push(']');
                }
                if let Some(ref outcome) = s.expected_outcome {
                    text.push_str(" -> ");
                    text.push_str(outcome);
                }
                text
            })
            .collect::<Vec<String>>()
            .join(". ")
    }

    /// Serialize an f32 slice to little-endian bytes for SQLite BLOB storage.
    fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
        let mut blob = Vec::with_capacity(embedding.len() * 4);
        for val in embedding {
            blob.extend_from_slice(&val.to_le_bytes());
        }
        blob
    }

    /// Deserialize a BLOB back to an f32 vector.
    fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
        let mut embedding = Vec::with_capacity(blob.len() / 4);
        for bytes in blob.chunks_exact(4) {
            embedding.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }
        embedding
    }

    /// Cosine similarity between two embedding vectors.
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

    // --- Search constants (same as SemanticStore) ---

    /// Maximum candidate procedures to load for cosine similarity when FTS5 pre-filter is used.
    const FTS_CANDIDATE_LIMIT: usize = 200;
    /// Fallback limit when FTS5 finds no matches — load most recent procedures instead.
    const RECENCY_FALLBACK_LIMIT: usize = 500;
    /// Reciprocal Rank Fusion constant (k=60 is standard).
    const RRF_K: f32 = 60.0;

    // ========================================================================
    // CRUD Operations
    // ========================================================================

    /// Store a new procedure. Generates a UUID if `procedure.id` is empty.
    ///
    /// Computes an embedding from the procedure's name, description, and step actions,
    /// then inserts both the procedure record and the FTS content record inside a
    /// transaction.
    ///
    /// Returns the procedure's UUID.
    pub async fn store(&self, procedure: &Procedure) -> Result<String, AgentOSError> {
        let proc_id = if procedure.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            procedure.id.clone()
        };

        // Build embedding text and compute vector
        let embed_text = Self::build_embedding_text(procedure);
        let embedding = self
            .embedder
            .embed(&[embed_text.as_str()])
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to compute procedure embedding: {}", e))
            })?
            .pop()
            .ok_or_else(|| {
                AgentOSError::StorageError(
                    "Procedure embedding returned empty result".to_string(),
                )
            })?;
        let blob = Self::embedding_to_blob(&embedding);

        // Serialize JSON fields
        let preconditions_json = serde_json::to_string(&procedure.preconditions)
            .unwrap_or_else(|_| "[]".to_string());
        let steps_json =
            serde_json::to_string(&procedure.steps).unwrap_or_else(|_| "[]".to_string());
        let postconditions_json = serde_json::to_string(&procedure.postconditions)
            .unwrap_or_else(|_| "[]".to_string());
        let source_episodes_json = serde_json::to_string(&procedure.source_episodes)
            .unwrap_or_else(|_| "[]".to_string());
        let tags_json =
            serde_json::to_string(&procedure.tags).unwrap_or_else(|_| "[]".to_string());
        let agent_id_str = procedure
            .agent_id
            .as_ref()
            .map(|id| id.as_uuid().to_string());

        let now = Utc::now().to_rfc3339();
        let steps_text = Self::build_steps_text(&procedure.steps);

        let conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock procedural db for store".to_string())
        })?;

        conn.execute("BEGIN TRANSACTION", [])
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        // Insert into main procedures table
        if let Err(e) = conn.execute(
            "INSERT INTO procedures (id, name, description, preconditions, steps, postconditions,
                                     success_count, failure_count, source_episodes, agent_id,
                                     tags, embedding, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                proc_id,
                procedure.name,
                procedure.description,
                preconditions_json,
                steps_json,
                postconditions_json,
                procedure.success_count,
                procedure.failure_count,
                source_episodes_json,
                agent_id_str,
                tags_json,
                blob,
                now,
                now,
            ],
        ) {
            conn.execute("ROLLBACK TRANSACTION", []).ok();
            return Err(AgentOSError::StorageError(format!(
                "Insert procedure failed: {}",
                e
            )));
        }

        // Insert into FTS content table (triggers auto-populate FTS index)
        if let Err(e) = conn.execute(
            "INSERT INTO procedures_fts_content (proc_id, name, description, steps_text)
             VALUES (?1, ?2, ?3, ?4)",
            params![proc_id, procedure.name, procedure.description, steps_text],
        ) {
            conn.execute("ROLLBACK TRANSACTION", []).ok();
            return Err(AgentOSError::StorageError(format!(
                "Insert FTS content failed: {}",
                e
            )));
        }

        conn.execute("COMMIT TRANSACTION", [])
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        Ok(proc_id)
    }

    /// Hybrid search for procedures using FTS5 pre-filter + cosine similarity + RRF fusion.
    ///
    /// The search pipeline mirrors `SemanticStore::search()`:
    /// 1. FTS5 MATCH query against `procedures_fts` to get BM25-ranked candidate rowids
    /// 2. Embed query and compute cosine similarity against candidate procedure embeddings
    /// 3. Fuse scores via RRF: 70% semantic + 30% FTS (normalized)
    /// 4. Return top-k results sorted by RRF score descending
    ///
    /// If FTS5 finds no matches (e.g. synonyms not in text), falls back to loading the
    /// most recent procedures (bounded by `RECENCY_FALLBACK_LIMIT`).
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

        // Embed query
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
            AgentOSError::StorageError("Failed to lock procedural db for search".to_string())
        })?;

        let agent_id_str = agent_id.map(|id| id.as_uuid().to_string());

        // Phase 1: FTS5 pre-filter — get candidate FTS content rowids by text relevance
        let fts_ranks: HashMap<String, f32> = {
            let mut fts_map = HashMap::new();
            if let Ok(mut fts_stmt) = conn.prepare(
                "SELECT c.proc_id, f.rank
                 FROM procedures_fts f
                 JOIN procedures_fts_content c ON f.rowid = c.rowid
                 WHERE procedures_fts MATCH ?1
                 ORDER BY f.rank
                 LIMIT ?2",
            ) {
                if let Ok(rows) = fts_stmt.query_map(
                    params![query, Self::FTS_CANDIDATE_LIMIT as i64],
                    |row| {
                        let proc_id: String = row.get(0)?;
                        let rank: f64 = row.get(1)?;
                        Ok((proc_id, rank as f32))
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

        // Phase 2: Load procedure records + embeddings for candidates
        let sql = if use_fts {
            let placeholders: Vec<String> = (0..fts_ranks.len())
                .map(|i| format!("?{}", i + 2))
                .collect();
            format!(
                "SELECT id, name, description, preconditions, steps, postconditions,
                        success_count, failure_count, source_episodes, agent_id,
                        tags, embedding, created_at, updated_at
                 FROM procedures
                 WHERE id IN ({})
                   AND (?1 IS NULL OR agent_id IS NULL OR agent_id = ?1)",
                placeholders.join(",")
            )
        } else {
            format!(
                "SELECT id, name, description, preconditions, steps, postconditions,
                        success_count, failure_count, source_episodes, agent_id,
                        tags, embedding, created_at, updated_at
                 FROM procedures
                 WHERE (?1 IS NULL OR agent_id IS NULL OR agent_id = ?1)
                 ORDER BY updated_at DESC
                 LIMIT {}",
                Self::RECENCY_FALLBACK_LIMIT
            )
        };

        // Build parameter list
        let mut param_values: Vec<rusqlite::types::Value> = Vec::new();
        param_values.push(match &agent_id_str {
            Some(s) => rusqlite::types::Value::from(s.clone()),
            None => rusqlite::types::Value::Null,
        });
        if use_fts {
            for proc_id in fts_ranks.keys() {
                param_values.push(rusqlite::types::Value::from(proc_id.clone()));
            }
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let proc_rows = stmt
            .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let description: String = row.get(2)?;
                let preconditions_json: String = row.get(3)?;
                let steps_json: String = row.get(4)?;
                let postconditions_json: String = row.get(5)?;
                let success_count: u32 = row.get(6)?;
                let failure_count: u32 = row.get(7)?;
                let source_episodes_json: String = row.get(8)?;
                let agent_id_val: Option<String> = row.get(9)?;
                let tags_json: String = row.get(10)?;
                let embedding_blob: Option<Vec<u8>> = row.get(11)?;
                let created_at_str: String = row.get(12)?;
                let updated_at_str: String = row.get(13)?;

                let preconditions: Vec<String> =
                    serde_json::from_str(&preconditions_json).unwrap_or_default();
                let steps: Vec<ProcedureStep> =
                    serde_json::from_str(&steps_json).unwrap_or_default();
                let postconditions: Vec<String> =
                    serde_json::from_str(&postconditions_json).unwrap_or_default();
                let source_episodes: Vec<String> =
                    serde_json::from_str(&source_episodes_json).unwrap_or_default();
                let tags: Vec<String> =
                    serde_json::from_str(&tags_json).unwrap_or_default();

                let parsed_agent_id = agent_id_val.and_then(|s| {
                    uuid::Uuid::parse_str(&s).ok().map(AgentID::from_uuid)
                });
                let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
                    .unwrap_or_else(|_| chrono::Local::now().into())
                    .with_timezone(&chrono::Utc);
                let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                    .unwrap_or_else(|_| chrono::Local::now().into())
                    .with_timezone(&chrono::Utc);

                let embedding = embedding_blob
                    .map(|b| Self::blob_to_embedding(&b))
                    .unwrap_or_default();

                Ok((
                    Procedure {
                        id,
                        name,
                        description,
                        preconditions,
                        steps,
                        postconditions,
                        success_count,
                        failure_count,
                        source_episodes,
                        agent_id: parsed_agent_id,
                        tags,
                        created_at,
                        updated_at,
                    },
                    embedding,
                ))
            })
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        // Phase 3: Score procedures with cosine + RRF fusion
        let mut results: Vec<ProcedureSearchResult> = Vec::new();
        for row in proc_rows {
            let (procedure, embedding) =
                row.map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            if embedding.len() != self.dimension {
                continue;
            }

            let semantic_score = Self::cosine_similarity(&query_embedding, &embedding);
            if semantic_score < min_score {
                continue;
            }

            // FTS score (negative rank from FTS5 — more negative = better match)
            let fts_score = fts_ranks
                .get(&procedure.id)
                .map(|r| -r)
                .unwrap_or(0.0);

            // RRF fusion: 70% semantic + 30% FTS (normalized)
            let rrf_score = if use_fts && fts_score > 0.0 {
                let fts_normalized = fts_score / (fts_score + Self::RRF_K);
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
    }

    /// Get a single procedure by ID.
    pub fn get(&self, id: &str) -> Result<Option<Procedure>, AgentOSError> {
        let conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock procedural db for get".to_string())
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, name, description, preconditions, steps, postconditions,
                        success_count, failure_count, source_episodes, agent_id,
                        tags, created_at, updated_at
                 FROM procedures WHERE id = ?1",
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![id], |row| Self::row_to_procedure(row))
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        match rows.next() {
            Some(Ok(procedure)) => Ok(Some(procedure)),
            Some(Err(e)) => Err(AgentOSError::StorageError(e.to_string())),
            None => Ok(None),
        }
    }

    /// Increment success or failure count and touch `updated_at`.
    pub fn update_stats(&self, id: &str, success: bool) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError(
                "Failed to lock procedural db for update_stats".to_string(),
            )
        })?;

        let now = Utc::now().to_rfc3339();
        let column = if success {
            "success_count"
        } else {
            "failure_count"
        };

        // Safe: `column` is a compile-time literal, not user input.
        let sql = format!(
            "UPDATE procedures SET {} = {} + 1, updated_at = ?1 WHERE id = ?2",
            column, column
        );

        let updated = conn
            .execute(&sql, params![now, id])
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to update procedure stats: {}", e))
            })?;

        if updated == 0 {
            return Err(AgentOSError::StorageError(format!(
                "Procedure '{}' not found",
                id
            )));
        }

        Ok(())
    }

    /// Delete a procedure and its FTS content (transactional).
    pub fn delete(&self, id: &str) -> Result<(), AgentOSError> {
        let mut conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock procedural db for delete".to_string())
        })?;

        let tx = conn.transaction().map_err(|e| {
            AgentOSError::StorageError(format!("Failed to start transaction: {}", e))
        })?;

        // Delete FTS content row first (trigger cleans FTS index)
        tx.execute(
            "DELETE FROM procedures_fts_content WHERE proc_id = ?1",
            params![id],
        )
        .map_err(|e| {
            AgentOSError::StorageError(format!("Failed to delete FTS content: {}", e))
        })?;

        let deleted = tx
            .execute("DELETE FROM procedures WHERE id = ?1", params![id])
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to delete procedure: {}", e))
            })?;

        if deleted == 0 {
            return Err(AgentOSError::StorageError(format!(
                "Procedure '{}' not found",
                id
            )));
        }

        tx.commit().map_err(|e| {
            AgentOSError::StorageError(format!("Failed to commit delete: {}", e))
        })?;

        Ok(())
    }

    /// List all procedures owned by a specific agent, ordered by most recently updated first.
    ///
    /// Includes global procedures (agent_id IS NULL) alongside agent-specific ones.
    pub fn list_by_agent(
        &self,
        agent_id: &AgentID,
    ) -> Result<Vec<Procedure>, AgentOSError> {
        let conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError(
                "Failed to lock procedural db for list_by_agent".to_string(),
            )
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, name, description, preconditions, steps, postconditions,
                        success_count, failure_count, source_episodes, agent_id,
                        tags, created_at, updated_at
                 FROM procedures
                 WHERE agent_id = ?1 OR agent_id IS NULL
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let proc_iter = stmt
            .query_map(
                params![agent_id.as_uuid().to_string()],
                |row| Self::row_to_procedure(row),
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let mut procedures = Vec::new();
        for row in proc_iter {
            procedures.push(
                row.map_err(|e| AgentOSError::StorageError(e.to_string()))?,
            );
        }

        Ok(procedures)
    }

    /// Delete procedures older than `max_age` and return the number deleted.
    pub fn sweep_old_entries(
        &self,
        max_age: std::time::Duration,
    ) -> Result<usize, AgentOSError> {
        let chrono_age = chrono::Duration::from_std(max_age).map_err(|e| {
            AgentOSError::StorageError(format!("Invalid max_age duration: {}", e))
        })?;
        let cutoff = (Utc::now() - chrono_age).to_rfc3339();

        let mut conn = self.conn.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock procedural db for sweep".to_string())
        })?;

        let tx = conn.transaction().map_err(|e| {
            AgentOSError::StorageError(format!("Failed to start sweep transaction: {}", e))
        })?;

        // Delete FTS content for expired procedures
        tx.execute(
            "DELETE FROM procedures_fts_content WHERE proc_id IN
               (SELECT id FROM procedures WHERE updated_at < ?1)",
            params![cutoff],
        )
        .map_err(|e| {
            AgentOSError::StorageError(format!("Failed to sweep FTS content: {}", e))
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
            AgentOSError::StorageError(format!("Failed to commit sweep: {}", e))
        })?;

        Ok(deleted)
    }

    /// Helper: convert a row (13-column SELECT without embedding) to a Procedure.
    ///
    /// Expected column order:
    /// id, name, description, preconditions, steps, postconditions,
    /// success_count, failure_count, source_episodes, agent_id,
    /// tags, created_at, updated_at
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
        let agent_id_val: Option<String> = row.get(9)?;
        let tags_json: String = row.get(10)?;
        let created_at_str: String = row.get(11)?;
        let updated_at_str: String = row.get(12)?;

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
            agent_id: agent_id_val
                .and_then(|s| uuid::Uuid::parse_str(&s).ok().map(AgentID::from_uuid)),
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            created_at: chrono::DateTime::parse_from_rfc3339(&created_at_str)
                .unwrap_or_else(|_| chrono::Local::now().into())
                .with_timezone(&chrono::Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                .unwrap_or_else(|_| chrono::Local::now().into())
                .with_timezone(&chrono::Utc),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedder::Embedder;
    use tempfile::TempDir;

    fn make_test_procedure(name: &str, description: &str) -> Procedure {
        Procedure {
            id: String::new(), // store() will generate UUID
            name: name.to_string(),
            description: description.to_string(),
            preconditions: vec!["Tests passing".to_string()],
            steps: vec![
                ProcedureStep {
                    order: 0,
                    action: "Run unit tests".to_string(),
                    tool: Some("shell-exec".to_string()),
                    expected_outcome: Some("All tests pass".to_string()),
                },
                ProcedureStep {
                    order: 1,
                    action: "Build Docker image".to_string(),
                    tool: Some("shell-exec".to_string()),
                    expected_outcome: Some("Image built successfully".to_string()),
                },
                ProcedureStep {
                    order: 2,
                    action: "Push to container registry".to_string(),
                    tool: Some("shell-exec".to_string()),
                    expected_outcome: None,
                },
                ProcedureStep {
                    order: 3,
                    action: "Apply Kubernetes manifest".to_string(),
                    tool: Some("shell-exec".to_string()),
                    expected_outcome: Some("Rollout complete".to_string()),
                },
            ],
            postconditions: vec!["Service is live".to_string()],
            success_count: 0,
            failure_count: 0,
            source_episodes: vec![],
            agent_id: None,
            tags: vec!["deployment".to_string(), "kubernetes".to_string()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_store_and_get_procedure() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let proc = make_test_procedure(
            "deploy-to-production",
            "Deploy a service to the production Kubernetes cluster",
        );
        let id = store.store(&proc).await.unwrap();
        assert!(!id.is_empty());

        let retrieved = store.get(&id).unwrap().expect("procedure should exist");
        assert_eq!(retrieved.name, "deploy-to-production");
        assert_eq!(retrieved.steps.len(), 4);
        assert_eq!(retrieved.steps[0].action, "Run unit tests");
        assert_eq!(retrieved.steps[0].tool, Some("shell-exec".to_string()));
        assert_eq!(retrieved.preconditions, vec!["Tests passing"]);
        assert_eq!(retrieved.postconditions, vec!["Service is live"]);
        assert_eq!(retrieved.tags, vec!["deployment", "kubernetes"]);
    }

    #[tokio::test]
    async fn test_search_finds_relevant_procedure() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let deploy_proc = make_test_procedure(
            "deploy-to-production",
            "Deploy a service to the production Kubernetes cluster",
        );
        store.store(&deploy_proc).await.unwrap();

        let backup_proc = Procedure {
            id: String::new(),
            name: "backup-database".to_string(),
            description: "Create a full backup of the PostgreSQL database".to_string(),
            preconditions: vec![],
            steps: vec![ProcedureStep {
                order: 0,
                action: "Run pg_dump with compression".to_string(),
                tool: Some("shell-exec".to_string()),
                expected_outcome: Some("Backup file created".to_string()),
            }],
            postconditions: vec!["Backup file exists on S3".to_string()],
            success_count: 0,
            failure_count: 0,
            source_episodes: vec![],
            agent_id: None,
            tags: vec!["database".to_string()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        store.store(&backup_proc).await.unwrap();

        let results = store
            .search("how to deploy to production Kubernetes", None, 5, 0.0)
            .await
            .unwrap();

        assert!(!results.is_empty());
        // The deployment procedure should rank higher than the backup procedure
        let deploy_score = results
            .iter()
            .find(|r| r.procedure.name == "deploy-to-production")
            .map(|r| r.rrf_score)
            .unwrap_or(0.0);
        let backup_score = results
            .iter()
            .find(|r| r.procedure.name == "backup-database")
            .map(|r| r.rrf_score)
            .unwrap_or(0.0);

        assert!(
            deploy_score > backup_score,
            "Expected deploy ({}) to rank higher than backup ({})",
            deploy_score,
            backup_score
        );
    }

    #[tokio::test]
    async fn test_search_respects_min_score() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let proc = make_test_procedure(
            "deploy-to-production",
            "Deploy a service to the production Kubernetes cluster",
        );
        store.store(&proc).await.unwrap();

        // With an impossibly high min_score, nothing should match
        let results = store
            .search("deploy", None, 5, 0.999)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_search_validates_min_score_range() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let err = store.search("test", None, 5, 1.5).await.unwrap_err();
        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    }

    #[tokio::test]
    async fn test_update_stats_success() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let proc = make_test_procedure("test-proc", "A test procedure");
        let id = store.store(&proc).await.unwrap();

        store.update_stats(&id, true).unwrap();
        store.update_stats(&id, true).unwrap();
        store.update_stats(&id, true).unwrap();
        store.update_stats(&id, false).unwrap();

        let retrieved = store.get(&id).unwrap().expect("should exist");
        assert_eq!(retrieved.success_count, 3);
        assert_eq!(retrieved.failure_count, 1);
    }

    #[tokio::test]
    async fn test_update_stats_not_found() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let err = store
            .update_stats("nonexistent-id", true)
            .unwrap_err();
        assert!(matches!(err, AgentOSError::StorageError(_)));
    }

    #[tokio::test]
    async fn test_delete_procedure() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let proc = make_test_procedure("to-delete", "Will be deleted");
        let id = store.store(&proc).await.unwrap();

        assert!(store.get(&id).unwrap().is_some());
        store.delete(&id).unwrap();
        assert!(store.get(&id).unwrap().is_none());
    }

    #[tokio::test]
    async fn test_delete_not_found() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let err = store.delete("nonexistent").unwrap_err();
        assert!(matches!(err, AgentOSError::StorageError(_)));
    }

    #[tokio::test]
    async fn test_list_by_agent() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let agent_a = AgentID::new();
        let agent_b = AgentID::new();

        // Global procedure (no agent)
        let global_proc = make_test_procedure("global-proc", "A global procedure");
        store.store(&global_proc).await.unwrap();

        // Agent A procedure
        let mut proc_a = make_test_procedure("agent-a-proc", "Agent A procedure");
        proc_a.agent_id = Some(agent_a);
        store.store(&proc_a).await.unwrap();

        // Agent B procedure
        let mut proc_b = make_test_procedure("agent-b-proc", "Agent B procedure");
        proc_b.agent_id = Some(agent_b);
        store.store(&proc_b).await.unwrap();

        // Agent A should see: global-proc + agent-a-proc
        let a_procs = store.list_by_agent(&agent_a).unwrap();
        let a_names: Vec<&str> = a_procs.iter().map(|p| p.name.as_str()).collect();
        assert!(a_names.contains(&"global-proc"));
        assert!(a_names.contains(&"agent-a-proc"));
        assert!(!a_names.contains(&"agent-b-proc"));

        // Agent B should see: global-proc + agent-b-proc
        let b_procs = store.list_by_agent(&agent_b).unwrap();
        let b_names: Vec<&str> = b_procs.iter().map(|p| p.name.as_str()).collect();
        assert!(b_names.contains(&"global-proc"));
        assert!(b_names.contains(&"agent-b-proc"));
        assert!(!b_names.contains(&"agent-a-proc"));
    }

    #[tokio::test]
    async fn test_search_top_k_zero_returns_empty() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let results = store.search("anything", None, 0, 0.0).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_get_nonexistent_returns_none() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let result = store.get("does-not-exist").unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_sweep_old_entries() {
        let dir = TempDir::new().unwrap();
        let embedder = Arc::new(Embedder::new().unwrap());
        let store = ProceduralStore::open_with_embedder(dir.path(), embedder).unwrap();

        let proc = make_test_procedure("old-proc", "Will be swept");
        store.store(&proc).await.unwrap();

        // Sweep with zero duration removes everything
        let deleted = store
            .sweep_old_entries(std::time::Duration::from_secs(0))
            .unwrap();
        assert_eq!(deleted, 1);
    }
}
```

---

### 4.3 Export from `lib.rs`

**File:** `crates/agentos-memory/src/lib.rs`

Add procedural module and re-exports:

```rust
pub mod embedder;
pub mod episodic;
pub mod procedural;
pub mod semantic;
pub mod types;

pub use embedder::Embedder;
pub use episodic::EpisodicStore;
pub use procedural::ProceduralStore;
pub use semantic::SemanticStore;
pub use types::{
    EpisodeType, EpisodicEntry, MemoryChunk, MemoryEntry, Procedure, ProcedureSearchResult,
    ProcedureStep, RecallQuery, RecallResult,
};
```

---

### 4.4 Wire into `ContextCompiler` (Phase 3 integration point)

**File:** `crates/agentos-kernel/src/context.rs`

This subtask is deferred until Phase 3 (`ContextCompiler`) is implemented. The integration point is:

1. Add `procedures: Vec<String>` field to `CompilationInputs`
2. In `compile()`, merge procedural summaries into the Knowledge budget category:

```rust
pub struct CompilationInputs {
    // ... existing fields from Phase 3 ...
    /// Formatted procedure summaries from ProceduralStore::search()
    pub procedures: Vec<String>,
}

// In compile():
let all_knowledge: Vec<String> = inputs.knowledge.into_iter()
    .chain(inputs.procedures.into_iter())
    .collect();
```

The caller (run loop or task executor) queries `ProceduralStore::search()` with the current task description, formats results as:

```
Procedure: {name}
{description}
Steps: 1. {step1} 2. {step2} ...
Success rate: {success_count}/{success_count + failure_count}
```

And passes the formatted strings as `procedures` in `CompilationInputs`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-memory/src/types.rs` | Add `Procedure`, `ProcedureStep`, `ProcedureSearchResult` structs |
| `crates/agentos-memory/src/procedural.rs` | **New** — `ProceduralStore` with `store()`, `search()`, `get()`, `update_stats()`, `delete()`, `list_by_agent()`, `sweep_old_entries()`, and 12 inline tests |
| `crates/agentos-memory/src/lib.rs` | Add `pub mod procedural;` and re-export `ProceduralStore`, `Procedure`, `ProcedureStep`, `ProcedureSearchResult` |
| `crates/agentos-kernel/src/context.rs` | *(Deferred to Phase 3 completion)* Add `procedures: Vec<String>` to `CompilationInputs` |

---

## Dependencies

- **Requires:** Phase 1 (episodic auto-write) — consolidation (Phase 7) will feed procedures from episodic patterns
- **Blocks:** Phase 5 (retrieval gate needs to know about procedural index), Phase 7 (consolidation writes procedures from episodic patterns), Phase 8 (agents manage procedure blocks)

**Crate dependencies** — no new crate deps required. `ProceduralStore` uses `rusqlite`, `serde`/`serde_json`, `chrono`, `uuid`, `agentos-types`, and `fastembed` (via `Embedder`) — all already in `agentos-memory/Cargo.toml`.

---

## Test Plan

All tests use `tempfile::TempDir` for filesystem isolation and `Arc<Embedder>` for shared embedding model access. Tests are `#[tokio::test]` because `store()` and `search()` are async.

| Test | Asserts |
|------|---------|
| `test_store_and_get_procedure` | Store a procedure, get by ID, verify all fields round-trip correctly (name, steps, preconditions, postconditions, tags) |
| `test_search_finds_relevant_procedure` | Store deploy + backup procedures, search for "deploy to production Kubernetes", verify deploy ranks higher than backup by RRF score |
| `test_search_respects_min_score` | Store a procedure, search with min_score=0.999, verify empty results |
| `test_search_validates_min_score_range` | Search with min_score=1.5, verify `SchemaValidation` error |
| `test_update_stats_success` | Store procedure, call `update_stats(true)` 3x and `update_stats(false)` 1x, verify counts are 3 and 1 |
| `test_update_stats_not_found` | Call `update_stats` on nonexistent ID, verify `StorageError` |
| `test_delete_procedure` | Store then delete, verify `get()` returns `None` |
| `test_delete_not_found` | Delete nonexistent ID, verify `StorageError` |
| `test_list_by_agent` | Store global + agent A + agent B procedures, verify agent A sees global + own, not B's |
| `test_search_top_k_zero_returns_empty` | Search with top_k=0, verify empty vec |
| `test_get_nonexistent_returns_none` | Get nonexistent ID, verify `None` |
| `test_sweep_old_entries` | Store procedure, sweep with 0s max_age, verify 1 deleted |

---

## Verification

```bash
# Build the memory crate (confirms types + procedural module compile)
cargo build -p agentos-memory

# Run all memory crate tests (includes new procedural tests)
cargo test -p agentos-memory

# Run only procedural tests
cargo test -p agentos-memory procedural

# Lint check
cargo clippy -p agentos-memory -- -D warnings

# Format check
cargo fmt --all -- --check

# Full workspace build (confirms no downstream breakage from new exports)
cargo build --workspace

# Full workspace tests
cargo test --workspace
```

---

## Security Notes

- All SQL uses `params![]` or `params_from_iter()` — zero string interpolation
- The only "dynamic SQL" is in `update_stats()` where the column name is a compile-time literal (`"success_count"` or `"failure_count"`), not user input
- The FTS content table uses a separate `procedures_fts_content` table with triggers, avoiding the `content=` directly on `procedures` which would cause FTS5 to try to read blob columns
- `agent_id` filtering in `search()` and `list_by_agent()` prevents cross-agent data leakage

---

## Related

- [[Memory Context Architecture Plan]] — master plan
- [[03-context-assembly-engine]] — Phase 3 (ContextCompiler integration point)
- [[05-adaptive-retrieval-gate]] — Phase 5 (must route to procedural index)
- [[07-consolidation-pathways]] — Phase 7 (writes procedures from episodic patterns)
- [[08-agent-memory-self-management]] — Phase 8 (agents CRUD their own procedures)
