---
title: "Phase 1: Core Storage Engine"
tags:
  - memory
  - scratchpad
  - v3
  - plan
date: 2026-03-23
status: complete
effort: 2d
priority: high
---

# Phase 1: Core Storage Engine

> Create the `agentos-scratch` crate with SQLite-backed page storage, CRUD operations, and FTS5 search.

---

## Why This Phase

Everything else depends on having a working storage layer. This phase establishes the `ScratchpadStore` — the SQLite-backed persistence layer that holds markdown pages, provides full-text search, and enforces per-agent namespacing. Without this, no tools, no links, no graph.

---

## Current → Target State

**Current:** `MemoryBlockStore` in `agentos-kernel/src/memory_blocks.rs` — flat key-value, 2KB limit, no FTS.

**Target:** New `agentos-scratch` crate with:
- `ScratchPage` struct (id, agent_id, title, content, metadata, tags, timestamps)
- `ScratchpadStore` — SQLite store with FTS5 index
- CRUD: `write_page`, `read_page`, `delete_page`, `list_pages`
- FTS5 search: `search(query, tags, limit)`
- Validation: 64KB content limit, 1000 pages/agent, unique title per agent

---

## Detailed Subtasks

### 1. Create crate skeleton

Create `crates/agentos-scratch/` with:

```
crates/agentos-scratch/
├── Cargo.toml
├── src/
│   ├── lib.rs        # Public exports
│   ├── types.rs      # ScratchPage, PageMetadata, SearchResult
│   ├── store.rs      # ScratchpadStore (SQLite CRUD + FTS5)
│   └── error.rs      # ScratchError enum
```

**`Cargo.toml` dependencies:**
```toml
[package]
name = "agentos-scratch"
version = "0.1.0"
edition = "2021"

[dependencies]
agentos-types = { path = "../agentos-types" }
rusqlite = { version = "0.31", features = ["bundled", "fts5"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
thiserror = "1"
tokio = { version = "1", features = ["rt"] }
tracing = "0.1"
```

### 2. Define types (`types.rs`)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchPage {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,  // Parsed frontmatter
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub page: ScratchPage,
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageSummary {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub updated_at: DateTime<Utc>,
}
```

### 3. Define errors (`error.rs`)

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScratchError {
    #[error("Page not found: {title} for agent {agent_id}")]
    PageNotFound { agent_id: String, title: String },

    #[error("Content too large: {size} bytes (max {max} bytes)")]
    ContentTooLarge { size: usize, max: usize },

    #[error("Too many pages for agent {agent_id}: {count} (max {max})")]
    TooManyPages { agent_id: String, count: usize, max: usize },

    #[error("Title too long: {length} chars (max {max})")]
    TitleTooLong { length: usize, max: usize },

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
```

### 4. Implement `ScratchpadStore` (`store.rs`)

```rust
pub struct ScratchpadStore {
    conn: Arc<Mutex<Connection>>,
}

impl ScratchpadStore {
    pub fn new(db_path: &Path) -> Result<Self, ScratchError>;
    fn init_schema(conn: &Connection) -> Result<(), ScratchError>;

    // CRUD
    pub async fn write_page(&self, agent_id: &str, title: &str, content: &str, tags: &[String]) -> Result<ScratchPage, ScratchError>;
    pub async fn read_page(&self, agent_id: &str, title: &str) -> Result<ScratchPage, ScratchError>;
    pub async fn delete_page(&self, agent_id: &str, title: &str) -> Result<(), ScratchError>;
    pub async fn list_pages(&self, agent_id: &str) -> Result<Vec<PageSummary>, ScratchError>;

    // Search
    pub async fn search(&self, agent_id: &str, query: &str, tags: &[String], limit: usize) -> Result<Vec<SearchResult>, ScratchError>;

    // Stats
    pub async fn page_count(&self, agent_id: &str) -> Result<usize, ScratchError>;
}
```

**Schema initialization:**
```sql
CREATE TABLE IF NOT EXISTS pages (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    metadata TEXT,
    tags TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(agent_id, title)
);

CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
    title, content, tags,
    content='pages',
    content_rowid='rowid'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER IF NOT EXISTS pages_ai AFTER INSERT ON pages BEGIN
    INSERT INTO pages_fts(rowid, title, content, tags)
    VALUES (new.rowid, new.title, new.content, new.tags);
END;

CREATE TRIGGER IF NOT EXISTS pages_ad AFTER DELETE ON pages BEGIN
    INSERT INTO pages_fts(pages_fts, rowid, title, content, tags)
    VALUES ('delete', old.rowid, old.title, old.content, old.tags);
END;

CREATE TRIGGER IF NOT EXISTS pages_au AFTER UPDATE ON pages BEGIN
    INSERT INTO pages_fts(pages_fts, rowid, title, content, tags)
    VALUES ('delete', old.rowid, old.title, old.content, old.tags);
    INSERT INTO pages_fts(rowid, title, content, tags)
    VALUES (new.rowid, new.title, new.content, new.tags);
END;

CREATE INDEX IF NOT EXISTS idx_pages_agent ON pages(agent_id);
```

### 5. Add to workspace

Add `"crates/agentos-scratch"` to the root `Cargo.toml` workspace members.

### 6. Write unit tests

Test cases:
- Write a page → read it back → content matches
- Write a page → write same title → upsert (updated_at changes)
- Write 64KB+1 content → `ContentTooLarge` error
- Write with tags → search by tag filter works
- FTS5 search finds pages by content keywords
- Delete page → read returns `PageNotFound`
- List pages returns all pages for agent, not other agents' pages
- Page count enforced at 1000 limit

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-scratch/Cargo.toml` | **New** — crate manifest |
| `crates/agentos-scratch/src/lib.rs` | **New** — public API exports |
| `crates/agentos-scratch/src/types.rs` | **New** — `ScratchPage`, `SearchResult`, `PageSummary` |
| `crates/agentos-scratch/src/store.rs` | **New** — `ScratchpadStore` with SQLite + FTS5 |
| `crates/agentos-scratch/src/error.rs` | **New** — `ScratchError` enum |
| `Cargo.toml` (root) | Add `"crates/agentos-scratch"` to workspace members |

---

## Dependencies

- **Requires:** Nothing — this is the foundation phase
- **Blocks:** Phase 2 (wikilink parser), Phase 3 (tools), Phase 6 (episodic auto-write)

---

## Test Plan

| Test | Assertion |
|------|-----------|
| `test_write_and_read` | Written page reads back with identical content, title, tags |
| `test_upsert` | Second write with same title updates content and `updated_at` |
| `test_content_too_large` | 64KB+1 returns `ScratchError::ContentTooLarge` |
| `test_page_limit` | 1001st page returns `ScratchError::TooManyPages` |
| `test_fts_search` | Search for keyword in content returns matching pages |
| `test_tag_filter` | Search with tag filter only returns pages with that tag |
| `test_delete` | Deleted page returns `PageNotFound` on read |
| `test_list_pages` | Lists only pages for the specified agent |
| `test_agent_isolation` | Agent A's pages not visible to Agent B's list/read |

---

## Verification

```bash
# Build the new crate
cargo build -p agentos-scratch

# Run tests
cargo test -p agentos-scratch

# Lint
cargo clippy -p agentos-scratch -- -D warnings

# Format
cargo fmt -p agentos-scratch -- --check

# Full workspace still builds
cargo build --workspace
cargo test --workspace
```

---

## Related

- [[Agent Scratchpad Plan]]
- [[02-wikilink-parser-and-backlinks]]
