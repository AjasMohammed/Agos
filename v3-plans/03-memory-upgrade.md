# Plan 03 — Memory Architecture Upgrade (`agentos-memory`)

## Goal

Upgrade AgentOS's memory from simple text-keyword matching to a **three-tier architecture** with genuine vector embeddings:

- **Tier 1**: Working memory (existing `ContextWindow` — unchanged)
- **Tier 2**: Episodic memory — full indexed SQLite task history with recall queries
- **Tier 3**: Semantic memory — on-device ONNX vector embeddings (all-MiniLM-L6-v2) replacing keyword search

---

## Current State (What Exists)

| Component       | Current Implementation                              | Gap                                                |
| --------------- | --------------------------------------------------- | -------------------------------------------------- |
| Working memory  | `ContextWindow` ring buffer                         | ✅ Good as-is                                      |
| Episodic memory | `EpisodicMemory` struct backed by SQLite            | ⚠️ Basic — no indexed recall, no cross-task search |
| Semantic memory | `memory-search` tool — text `LIKE` search in SQLite | ❌ No embeddings, no real semantic similarity      |

---

## Dependencies

```toml
# New workspace dependencies
fastembed  = "4"       # Local ONNX embeddings — no API key needed
                       # Bundles all-MiniLM-L6-v2, BGE-small-EN, etc.
```

`fastembed` downloads the embedding model on first use (~23MB for MiniLM). Subsequent runs use the cached model. No GPU required — runs on CPU.

---

## New Crate: `agentos-memory`

```
crates/agentos-memory/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── episodic.rs       # EpisodicStore — SQLite-backed task history with FTS5
    ├── semantic.rs       # SemanticStore — vector embeddings + cosine similarity
    ├── embedder.rs       # EmbeddingModel wrapper (fastembed)
    └── types.rs          # MemoryEntry, EpisodicEntry, RecallQuery, RecallResult
```

---

## Tier 2: Episodic Memory Upgrade

### Goal

Record every intent message, tool call, and LLM response in a searchable per-task SQLite database. Allow agents to recall events from their own task history and (with permission) from other tasks.

### Schema

```sql
-- Single shared table with task_id column for per-task queries
CREATE TABLE episodic_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id     TEXT NOT NULL,
    agent_id    TEXT NOT NULL,
    event_type  TEXT NOT NULL,             -- "intent" | "tool_call" | "tool_result" | "llm_response" | "agent_message"
    content     TEXT NOT NULL,             -- JSON-serialised event
    summary     TEXT,                      -- Human-readable one-line summary
    created_at  TEXT NOT NULL,             -- ISO 8601 UTC
    trace_id    TEXT
);

-- Full-text search over summary + content
CREATE VIRTUAL TABLE episodic_fts USING fts5(
    summary,
    content,
    content='episodic_events',
    content_rowid='id'
);

CREATE INDEX IF NOT EXISTS idx_episodes_task ON episodic_events(task_id);
CREATE INDEX IF NOT EXISTS idx_episodes_agent ON episodic_events(agent_id);
CREATE INDEX IF NOT EXISTS idx_episodes_type ON episodic_events(event_type);
CREATE INDEX IF NOT EXISTS idx_episodes_created ON episodic_events(created_at);
```

### EpisodicStore API

```rust
pub struct EpisodicStore {
    conn: Arc<Mutex<Connection>>,
}

impl EpisodicStore {
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError>;

    /// Record any kernel event into episodic memory.
    pub fn record(&self, entry: EpisodicEntry) -> Result<(), AgentOSError>;

    /// Full-text search across a task's event history.
    /// Verifies caller_agent_id owns the task or has explicit read permission.
    pub fn recall_task(
        &self,
        task_id: &TaskID,
        caller_agent_id: &AgentID,
        query: &str,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError>;

    /// Search across ALL tasks — requires `memory.episodic:r` capability token.
    /// The caller's token is checked via `CapabilityEngine::check()` before execution.
    pub fn recall_global(
        &self,
        query: &str,
        agent_id: Option<&AgentID>,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>, AgentOSError>;

    /// Get complete history for a task (for context rebuilding / rollback).
    pub fn task_history(&self, task_id: &TaskID) -> Result<Vec<EpisodicEntry>, AgentOSError>;
}
```

### Kernel Integration

Every time the kernel processes an event, it records to episodic memory:

```rust
// In kernel.rs — after handling any significant event:
self.episodic.record(EpisodicEntry {
    task_id: task.id.clone(),
    agent_id: task.agent_id.clone(),
    event_type: "tool_call".to_string(),
    content: serde_json::to_string(&serde_json::json!({ "tool": tool_name, "payload": payload }))?,
    summary: format!("Called tool '{}' with {} fields", tool_name, payload.as_object().map(|o| o.len()).unwrap_or(0)),
    trace_id: context.trace_id.clone(),
    ..Default::default()
}).ok(); // Best-effort — never panic on memory write
```

---

## Tier 3: Semantic Memory Upgrade (Vector Embeddings)

### Goal

Replace the current keyword-based `LIKE %query%` search with genuine semantic similarity search using ONNX embeddings, enabling an agent to search for concepts not just keywords.

### EmbeddingModel

```rust
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Lazily downloads the model on first call (~23MB for MiniLM).
    pub fn new() -> Result<Self, anyhow::Error> {
        let model = TextEmbedding::try_new(InitOptions {
            model_name: EmbeddingModel::AllMiniLML6V2,
            show_download_progress: true,
            ..Default::default()
        })?;
        Ok(Self { model })
    }

    /// Embed one or many texts — batched for efficiency.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, anyhow::Error> {
        self.model.embed(texts.to_vec(), None)
    }
}
```

### SemanticStore Schema

```sql
CREATE TABLE semantic_memory (
    id          TEXT PRIMARY KEY,
    agent_id    TEXT,                     -- NULL = global/shared
    key         TEXT NOT NULL,            -- Semantic key / title
    content     TEXT NOT NULL,            -- Original text content
    embedding   BLOB NOT NULL,            -- f32 vector, serialised as little-endian bytes
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    tags        TEXT                      -- JSON array of tags
);

CREATE INDEX idx_semantic_agent ON semantic_memory(agent_id);
CREATE INDEX idx_semantic_key ON semantic_memory(key);
```

### SemanticStore API

```rust
pub struct SemanticStore {
    conn: Arc<Mutex<Connection>>,
    embedder: Arc<Embedder>,
    dimension: usize,    // 384 for MiniLM-L6-v2
}

impl SemanticStore {
    /// Opens the semantic store and validates the embedding dimension
    /// by running a probe embedding on init. Returns an error if the
    /// model produces vectors of an unexpected size.
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError>;

    /// Write a memory entry — computes embedding automatically.
    pub async fn write(
        &self,
        key: &str,
        content: &str,
        agent_id: Option<&AgentID>,
        tags: &[&str],
    ) -> Result<MemoryID, AgentOSError>;

    /// Semantic search — returns top-k entries by cosine similarity.
    pub async fn search(
        &self,
        query: &str,
        agent_id: Option<&AgentID>,   // None = search all
        top_k: usize,
        min_score: f32,               // Cosine similarity threshold [0.0, 1.0]
    ) -> Result<Vec<RecallResult>, AgentOSError>;

    /// Exact key lookup.
    pub fn get_by_key(&self, key: &str) -> Result<Option<MemoryEntry>, AgentOSError>;

    /// Delete a memory entry.
    pub fn delete(&self, id: &MemoryID) -> Result<(), AgentOSError>;
}

pub struct RecallResult {
    pub entry: MemoryEntry,
    pub score: f32,    // Cosine similarity [0.0, 1.0]
}
```

### Cosine Similarity (In-Process)

Since the embedding dimension is small (384), in-process cosine similarity over the full dataset is fast enough for thousands of entries:

```rust
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (norm_a * norm_b) }
}
```

For >100k entries, upgrade to `usearch` or `qdrant` (future Phase 4 option).

---

## Updated Tool Implementations

### `memory-write` (updated)

Before: Stores raw text in SQLite.
After: Computes embedding via `Embedder`, stores vector + text in `SemanticStore`.

```rust
async fn execute(&self, payload: serde_json::Value, ctx: ToolExecutionContext)
    -> Result<serde_json::Value, AgentOSError>
{
    let key = payload["key"].as_str().ok_or(...)?;
    let content = payload["content"].as_str().ok_or(...)?;
    let tags: Vec<&str> = payload["tags"].as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let id = self.store.write(key, content, Some(&ctx.agent_id), &tags).await?;
    Ok(json!({ "status": "written", "id": id }))
}
```

### `memory-search` (updated)

Before: `SELECT ... WHERE content LIKE '%query%'`.
After: Embed query → cosine similarity search → return top-k with scores.

```rust
async fn execute(&self, payload: serde_json::Value, ctx: ToolExecutionContext)
    -> Result<serde_json::Value, AgentOSError>
{
    let query = payload["query"].as_str().ok_or(...)?;
    let top_k = payload["top_k"].as_u64().unwrap_or(5) as usize;
    let results = self.store.search(query, Some(&ctx.agent_id), top_k, 0.3).await?;
    Ok(json!({ "results": results }))
}
```

---

## Tests

```rust
// embedder tests
#[test]
fn test_embed_single_text_returns_correct_dimension() {
    let embedder = Embedder::new().unwrap();
    let vecs = embedder.embed(&["hello world"]).unwrap();
    assert_eq!(vecs[0].len(), 384); // MiniLM-L6-v2
}

// semantic search — assert relative ranking, not exact position
#[tokio::test]
async fn test_semantic_search_finds_similar_content() {
    let store = SemanticStore::open(&temp_dir()).unwrap();
    store.write("deployment", "We deploy our app using Docker containers.", None, &[]).await.unwrap();
    store.write("weather", "Today it is sunny and warm.", None, &[]).await.unwrap();
    let results = store.search("Kubernetes container deployment", None, 3, 0.2).await.unwrap();
    // Deployment entry should score higher than weather
    let deployment_score = results.iter().find(|r| r.entry.key == "deployment").map(|r| r.score);
    let weather_score = results.iter().find(|r| r.entry.key == "weather").map(|r| r.score);
    assert!(deployment_score > weather_score,
        "Expected 'deployment' to rank higher than 'weather'");
}

// episodic recall — full record + search with required fields
#[test]
fn test_episodic_fts_finds_tool_call() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = EpisodicStore::open(dir.path()).unwrap();
    let task_id = TaskID::new();
    let agent_id = AgentID::new();
    let trace_id = TraceID::new();
    store.record(
        &task_id, &agent_id, EpisodeType::ToolCall,
        r#"{"tool":"file-reader","path":"report.txt"}"#,
        Some("Called file-reader for report.txt"),
        None, &trace_id,
    ).unwrap();
    let results = store.search_events("file-reader", Some(&task_id), None, 10).unwrap();
    assert!(!results.is_empty());
    assert!(results[0].summary.as_deref().unwrap().contains("file-reader"));
}
```

---

## Verification

```bash
# Test semantic search
agentctl perm grant analyst memory.semantic:rw
agentctl task run --agent analyst \
  "Remember this: The deployment pipeline uses blue-green strategy with 5min health checks."

agentctl task run --agent analyst \
  "Search your memory for anything related to deployment strategies."
# Should return the entry above even without exact keyword match
```

> [!NOTE]
> fastembed downloads ~23MB on first run. This should be cached in `{data_dir}/models/`. Configure the path via `config.memory.model_cache_dir`. In Docker, this directory should be a mounted volume.
