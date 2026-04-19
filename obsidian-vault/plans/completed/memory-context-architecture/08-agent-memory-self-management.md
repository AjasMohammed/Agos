---
title: "Phase 8: Agent Memory Self-Management"
tags:
  - kernel
  - memory
  - tools
  - plan
  - v3
date: 2026-03-12
status: complete
effort: 2d
priority: medium
---

# Phase 8: Agent Memory Self-Management

> Give agents explicit CRUD control over their own persistent memory blocks, following the Letta/MemGPT self-editing memory pattern, so that agents make semantic judgments about what to remember instead of relying on framework heuristics.

---

## Why This Phase

The Letta (formerly MemGPT) research demonstrates that agent-driven memory management consistently outperforms framework-managed approaches. When agents can explicitly `read`, `write`, `list`, and `delete` their own memory blocks, they make contextual decisions about importance that rule-based eviction and summarization miss entirely. Key findings from the Letta architecture:

1. **Semantic judgment**: An LLM can determine "this user preference matters more than that intermediate calculation" in ways that importance scores and recency heuristics cannot.
2. **Persistent identity**: Named memory blocks (e.g., `persona`, `user_preferences`, `task_notes`) give agents stable working state that survives across task boundaries, enabling multi-session coherence.
3. **Bounded context cost**: Each block is capped at 2048 characters, so agents must distill knowledge to its essence rather than dumping raw conversation history into memory.
4. **Archival offload**: When blocks fill up, agents can push overflow content into long-term archival storage (the existing `SemanticStore`) and retrieve it later via search, creating a two-tier memory hierarchy that mirrors human short-term/long-term memory.

Without this phase, agents have no mechanism to persist structured working state across tasks. The existing `memory-write` and `memory-search` tools operate on the shared `SemanticStore` and `EpisodicStore`, which are global and unstructured. Agents cannot inspect what they "know" or selectively update their own state.

---

## Current State

| Component | Status |
|-----------|--------|
| `memory-write` tool | Exists; writes to shared `SemanticStore` or `EpisodicStore` |
| `memory-search` tool | Exists; searches shared stores |
| Per-agent memory blocks | Does not exist |
| `MemoryBlockStore` | Does not exist |
| Block CRUD tools | Do not exist |
| Context injection of blocks | Does not exist |
| Archival tools (agent-scoped) | Do not exist |

## Target State

| Component | Status |
|-----------|--------|
| `MemoryBlock` type | New struct in `agentos-types/src/context.rs` |
| `MemoryBlockStore` | New SQLite-backed store in `agentos-kernel/src/memory_blocks.rs` |
| `memory-block-read` tool | New tool; reads agent's own blocks |
| `memory-block-write` tool | New tool; creates/updates agent's blocks (upsert by label) |
| `memory-block-list` tool | New tool; lists agent's block labels and sizes |
| `memory-block-delete` tool | New tool; deletes a block by label |
| `archival-insert` tool | New tool; delegates to `SemanticStore.write()` scoped to agent |
| `archival-search` tool | New tool; delegates to `SemanticStore.recall()` scoped to agent |
| Context injection | `ContextManager` injects agent's blocks as a pinned System entry |
| Kernel integration | `Kernel` struct gains `memory_block_store: Arc<MemoryBlockStore>` field |

---

## Subtasks

### 8.1 Define `MemoryBlock` type

**File:** `crates/agentos-types/src/context.rs`

Add at the bottom of the file, before the `#[cfg(test)]` module:

```rust
use chrono::{DateTime, Utc};

/// A named, size-limited memory block scoped to a single agent.
///
/// Agents use these blocks to maintain persistent working state across tasks.
/// Each block has a human-readable label (e.g. "persona", "user_preferences")
/// and content capped at 2048 characters.
///
/// Follows the Letta/MemGPT self-editing memory pattern: agents CRUD their own
/// blocks via tools, and blocks are injected into context at prompt assembly time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlock {
    /// Unique identifier (UUID string).
    pub id: String,
    /// Human-readable label. Unique per agent — used as the upsert key.
    /// Examples: "persona", "user_preferences", "task_notes", "scratchpad".
    pub label: String,
    /// Block content. Maximum `MAX_SIZE` characters (2048).
    pub content: String,
    /// The agent that owns this block. Agents can only access their own blocks.
    pub agent_id: AgentID,
    /// When this block was first created.
    pub created_at: DateTime<Utc>,
    /// When this block's content was last updated.
    pub updated_at: DateTime<Utc>,
}

impl MemoryBlock {
    /// Maximum content size in characters.
    pub const MAX_SIZE: usize = 2048;

    /// Validate that the block's content does not exceed the size limit.
    pub fn validate_size(&self) -> Result<(), crate::error::AgentOSError> {
        if self.content.len() > Self::MAX_SIZE {
            return Err(crate::error::AgentOSError::InvalidInput(format!(
                "Memory block '{}' exceeds max size ({} > {} chars)",
                self.label,
                self.content.len(),
                Self::MAX_SIZE,
            )));
        }
        Ok(())
    }
}
```

**Also in `crates/agentos-types/src/lib.rs`**, add the re-export:

```rust
pub use context::MemoryBlock;
```

### 8.2 Create `MemoryBlockStore` (SQLite-backed)

**File:** `crates/agentos-kernel/src/memory_blocks.rs` (new file)

SQLite is the DEFAULT persistence backend. The in-memory mode is for tests only. All queries use parameterized bindings (project security rule: no SQL string interpolation).

```rust
use agentos_types::{AgentID, AgentOSError, MemoryBlock};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

/// Persistent store for per-agent memory blocks.
///
/// Backed by SQLite with parameterized queries. Each agent's blocks are
/// isolated — the store enforces agent scoping at the query level so no
/// agent can read or modify another agent's blocks.
pub struct MemoryBlockStore {
    db: Mutex<Connection>,
}

impl MemoryBlockStore {
    /// Open (or create) the memory block database at `{data_dir}/memory_blocks.db`.
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError> {
        let db_path = data_dir.join("memory_blocks.db");
        let conn = Connection::open(&db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open memory block DB: {}", e))
        })?;
        Self::init_schema(&conn)?;
        Ok(Self {
            db: Mutex::new(conn),
        })
    }

    /// Create an in-memory store (for tests only).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, AgentOSError> {
        let conn = Connection::open_in_memory().map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open in-memory DB: {}", e))
        })?;
        Self::init_schema(&conn)?;
        Ok(Self {
            db: Mutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), AgentOSError> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS memory_blocks (
                id         TEXT PRIMARY KEY,
                agent_id   TEXT NOT NULL,
                label      TEXT NOT NULL,
                content    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(agent_id, label)
            );

            CREATE INDEX IF NOT EXISTS idx_mb_agent
                ON memory_blocks(agent_id);
            CREATE INDEX IF NOT EXISTS idx_mb_agent_label
                ON memory_blocks(agent_id, label);
            ",
        )
        .map_err(|e| {
            AgentOSError::StorageError(format!(
                "Failed to initialize memory_blocks schema: {}",
                e
            ))
        })
    }

    /// List all blocks belonging to `agent_id`.
    pub fn list(&self, agent_id: &AgentID) -> Result<Vec<MemoryBlock>, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory block DB".to_string())
        })?;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, label, content, created_at, updated_at
                 FROM memory_blocks
                 WHERE agent_id = ?1
                 ORDER BY label ASC",
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to prepare list query: {}", e))
            })?;

        let rows = stmt
            .query_map(params![agent_id.as_uuid().to_string()], |row| {
                Ok(Self::row_to_block(row))
            })
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to list memory blocks: {}", e))
            })?;

        let mut blocks = Vec::new();
        for row in rows {
            let block = row.map_err(|e| {
                AgentOSError::StorageError(format!("Failed to parse block row: {}", e))
            })?;
            blocks.push(block);
        }
        Ok(blocks)
    }

    /// Get a specific block by agent and label.
    pub fn get(
        &self,
        agent_id: &AgentID,
        label: &str,
    ) -> Result<Option<MemoryBlock>, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory block DB".to_string())
        })?;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, label, content, created_at, updated_at
                 FROM memory_blocks
                 WHERE agent_id = ?1 AND label = ?2",
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to prepare get query: {}", e))
            })?;

        let result = stmt
            .query_row(
                params![agent_id.as_uuid().to_string(), label],
                |row| Ok(Self::row_to_block(row)),
            )
            .optional()
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to get memory block: {}", e))
            })?;

        Ok(result)
    }

    /// Create or update a block (upsert by agent_id + label).
    ///
    /// If a block with the same label already exists for this agent, its content
    /// and `updated_at` timestamp are replaced. Otherwise a new block is inserted.
    ///
    /// Returns the resulting `MemoryBlock`.
    pub fn write(
        &self,
        agent_id: &AgentID,
        label: &str,
        content: &str,
    ) -> Result<MemoryBlock, AgentOSError> {
        // Validate size before touching the database.
        if content.len() > MemoryBlock::MAX_SIZE {
            return Err(AgentOSError::InvalidInput(format!(
                "Memory block '{}' exceeds max size ({} > {} chars)",
                label,
                content.len(),
                MemoryBlock::MAX_SIZE,
            )));
        }

        // Validate label is non-empty and reasonable length.
        if label.is_empty() || label.len() > 128 {
            return Err(AgentOSError::InvalidInput(
                "Memory block label must be 1-128 characters".to_string(),
            ));
        }

        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory block DB".to_string())
        })?;

        let now = Utc::now().to_rfc3339();
        let agent_id_str = agent_id.as_uuid().to_string();

        // Check if block already exists for this agent + label.
        let existing_id: Option<String> = conn
            .query_row(
                "SELECT id FROM memory_blocks WHERE agent_id = ?1 AND label = ?2",
                params![agent_id_str, label],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to check existing block: {}", e))
            })?;

        let (block_id, created_at) = if let Some(existing) = existing_id {
            // Update existing block.
            conn.execute(
                "UPDATE memory_blocks SET content = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![content, now, existing],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to update memory block: {}", e))
            })?;

            // Retrieve original created_at.
            let created: String = conn
                .query_row(
                    "SELECT created_at FROM memory_blocks WHERE id = ?1",
                    params![existing],
                    |row| row.get(0),
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!(
                        "Failed to read created_at: {}",
                        e
                    ))
                })?;
            (existing, created)
        } else {
            // Insert new block.
            let new_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO memory_blocks (id, agent_id, label, content, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![new_id, agent_id_str, label, content, now, now],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to insert memory block: {}", e))
            })?;
            (new_id, now.clone())
        };

        let created_dt = chrono::DateTime::parse_from_rfc3339(&created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let updated_dt = chrono::DateTime::parse_from_rfc3339(&now)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        Ok(MemoryBlock {
            id: block_id,
            label: label.to_string(),
            content: content.to_string(),
            agent_id: *agent_id,
            created_at: created_dt,
            updated_at: updated_dt,
        })
    }

    /// Delete a block by agent and label. Returns `true` if a block was deleted.
    pub fn delete(
        &self,
        agent_id: &AgentID,
        label: &str,
    ) -> Result<bool, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory block DB".to_string())
        })?;
        let deleted = conn
            .execute(
                "DELETE FROM memory_blocks WHERE agent_id = ?1 AND label = ?2",
                params![agent_id.as_uuid().to_string(), label],
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to delete memory block: {}", e))
            })?;
        Ok(deleted > 0)
    }

    /// Format all blocks for injection into the agent's context window.
    ///
    /// Returns an empty string if the agent has no blocks. Otherwise returns
    /// a formatted string with each block as `[label]\ncontent`.
    pub fn blocks_for_context(
        &self,
        agent_id: &AgentID,
    ) -> Result<String, AgentOSError> {
        let blocks = self.list(agent_id)?;
        if blocks.is_empty() {
            return Ok(String::new());
        }
        let formatted = blocks
            .iter()
            .map(|b| format!("[{}]\n{}", b.label, b.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(formatted)
    }

    /// Count total blocks for an agent.
    pub fn count(&self, agent_id: &AgentID) -> Result<usize, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory block DB".to_string())
        })?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_blocks WHERE agent_id = ?1",
                params![agent_id.as_uuid().to_string()],
                |row| row.get(0),
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to count blocks: {}", e))
            })?;
        Ok(count as usize)
    }

    fn row_to_block(row: &rusqlite::Row) -> MemoryBlock {
        let id: String = row.get(0).unwrap_or_default();
        let agent_id_str: String = row.get(1).unwrap_or_default();
        let label: String = row.get(2).unwrap_or_default();
        let content: String = row.get(3).unwrap_or_default();
        let created_str: String = row.get(4).unwrap_or_default();
        let updated_str: String = row.get(5).unwrap_or_default();

        let agent_id = AgentID::from_uuid(
            uuid::Uuid::parse_str(&agent_id_str).unwrap_or_default(),
        );
        let created_at = chrono::DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        MemoryBlock {
            id,
            label,
            content,
            agent_id,
            created_at,
            updated_at,
        }
    }
}
```

**Register module in `crates/agentos-kernel/src/lib.rs`:**

```rust
pub mod memory_blocks;
```

### 8.3 Wire `MemoryBlockStore` into the Kernel

**File:** `crates/agentos-kernel/src/kernel.rs`

Add a new field to the `Kernel` struct:

```rust
pub memory_block_store: Arc<crate::memory_blocks::MemoryBlockStore>,
```

In `Kernel::boot()`, after opening `episodic_memory`, initialize the store:

```rust
let memory_block_store = Arc::new(
    crate::memory_blocks::MemoryBlockStore::open(&data_dir)
        .map_err(|e| anyhow::anyhow!("Memory block store init failed: {}", e))?,
);
```

Add `memory_block_store` to the `Kernel { ... }` struct literal.

### 8.4 Create tool manifests

**File:** `tools/core/memory-block-read.toml`

```toml
[manifest]
name        = "memory-block-read"
version     = "1.0.0"
description = "Read your memory blocks. Returns a specific block by label, or all blocks if no label given."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.blocks:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "MemoryBlockReadIntent"
output = "MemoryBlockContent"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 1000
syscalls      = []
```

**File:** `tools/core/memory-block-write.toml`

```toml
[manifest]
name        = "memory-block-write"
version     = "1.0.0"
description = "Create or update one of your memory blocks. Provide a label and content (max 2048 chars). If a block with that label exists, it is replaced."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.blocks:w"]

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "MemoryBlockWriteIntent"
output = "MemoryBlockWriteResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 2000
syscalls      = []
```

**File:** `tools/core/memory-block-list.toml`

```toml
[manifest]
name        = "memory-block-list"
version     = "1.0.0"
description = "List all your memory block labels with their sizes. Does not return content — use memory-block-read for that."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.blocks:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "MemoryBlockListIntent"
output = "MemoryBlockListResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 1000
syscalls      = []
```

**File:** `tools/core/memory-block-delete.toml`

```toml
[manifest]
name        = "memory-block-delete"
version     = "1.0.0"
description = "Delete one of your memory blocks by label. The block is permanently removed."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.blocks:w"]

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "MemoryBlockDeleteIntent"
output = "MemoryBlockDeleteResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 1000
syscalls      = []
```

**File:** `tools/core/archival-insert.toml`

```toml
[manifest]
name        = "archival-insert"
version     = "1.0.0"
description = "Insert content into your long-term archival memory. Use this when your memory blocks are full and you need to offload knowledge for later retrieval via archival-search."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.semantic:w"]

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "ArchivalInsertIntent"
output = "ArchivalInsertResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 64
max_cpu_ms    = 5000
syscalls      = []
```

**File:** `tools/core/archival-search.toml`

```toml
[manifest]
name        = "archival-search"
version     = "1.0.0"
description = "Search your long-term archival memory by keyword or natural language query. Returns matching entries sorted by relevance."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.semantic:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "ArchivalSearchIntent"
output = "ArchivalSearchResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 128
max_cpu_ms    = 10000
syscalls      = []
```

### 8.5 Implement tool handlers

**File:** `crates/agentos-tools/src/memory_block_tools.rs` (new file)

Each tool implements the `AgentTool` trait. Agent scoping is enforced by extracting `agent_id` from `ToolExecutionContext` and passing it to every `MemoryBlockStore` call. There is no way for a tool to specify a different agent's ID.

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_kernel::memory_blocks::MemoryBlockStore;
use agentos_types::*;
use async_trait::async_trait;
use std::sync::Arc;

// ---------- memory-block-read ----------

pub struct MemoryBlockReadTool {
    store: Arc<MemoryBlockStore>,
}

impl MemoryBlockReadTool {
    pub fn new(store: Arc<MemoryBlockStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for MemoryBlockReadTool {
    fn name(&self) -> &str {
        "memory-block-read"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.blocks".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let agent_id = &context.agent_id;

        if let Some(label) = payload.get("label").and_then(|v| v.as_str()) {
            // Read a specific block.
            match self.store.get(agent_id, label)? {
                Some(block) => Ok(serde_json::json!({
                    "label": block.label,
                    "content": block.content,
                    "created_at": block.created_at.to_rfc3339(),
                    "updated_at": block.updated_at.to_rfc3339(),
                })),
                None => Ok(serde_json::json!({
                    "error": format!("No memory block with label '{}'", label),
                })),
            }
        } else {
            // Read all blocks.
            let blocks = self.store.list(agent_id)?;
            let items: Vec<serde_json::Value> = blocks
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "label": b.label,
                        "content": b.content,
                        "created_at": b.created_at.to_rfc3339(),
                        "updated_at": b.updated_at.to_rfc3339(),
                    })
                })
                .collect();
            Ok(serde_json::json!({
                "blocks": items,
                "count": items.len(),
            }))
        }
    }
}

// ---------- memory-block-write ----------

pub struct MemoryBlockWriteTool {
    store: Arc<MemoryBlockStore>,
}

impl MemoryBlockWriteTool {
    pub fn new(store: Arc<MemoryBlockStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for MemoryBlockWriteTool {
    fn name(&self) -> &str {
        "memory-block-write"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.blocks".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let label = payload
            .get("label")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::InvalidInput(
                    "memory-block-write requires 'label' field".to_string(),
                )
            })?;
        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::InvalidInput(
                    "memory-block-write requires 'content' field".to_string(),
                )
            })?;

        let block = self.store.write(&context.agent_id, label, content)?;

        Ok(serde_json::json!({
            "success": true,
            "label": block.label,
            "size": block.content.len(),
            "max_size": agentos_types::MemoryBlock::MAX_SIZE,
            "updated_at": block.updated_at.to_rfc3339(),
        }))
    }
}

// ---------- memory-block-list ----------

pub struct MemoryBlockListTool {
    store: Arc<MemoryBlockStore>,
}

impl MemoryBlockListTool {
    pub fn new(store: Arc<MemoryBlockStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for MemoryBlockListTool {
    fn name(&self) -> &str {
        "memory-block-list"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.blocks".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let blocks = self.store.list(&context.agent_id)?;
        let items: Vec<serde_json::Value> = blocks
            .iter()
            .map(|b| {
                serde_json::json!({
                    "label": b.label,
                    "size": b.content.len(),
                    "updated_at": b.updated_at.to_rfc3339(),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "blocks": items,
            "count": items.len(),
        }))
    }
}

// ---------- memory-block-delete ----------

pub struct MemoryBlockDeleteTool {
    store: Arc<MemoryBlockStore>,
}

impl MemoryBlockDeleteTool {
    pub fn new(store: Arc<MemoryBlockStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for MemoryBlockDeleteTool {
    fn name(&self) -> &str {
        "memory-block-delete"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.blocks".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let label = payload
            .get("label")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::InvalidInput(
                    "memory-block-delete requires 'label' field".to_string(),
                )
            })?;

        let deleted = self.store.delete(&context.agent_id, label)?;

        Ok(serde_json::json!({
            "success": deleted,
            "label": label,
            "message": if deleted {
                format!("Block '{}' deleted", label)
            } else {
                format!("No block with label '{}' found", label)
            },
        }))
    }
}
```

**File:** `crates/agentos-tools/src/archival_tools.rs` (new file)

The archival tools delegate to the existing `SemanticStore`, scoped to the requesting agent's ID via `ToolExecutionContext.agent_id`.

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::SemanticStore;
use agentos_types::*;
use async_trait::async_trait;
use std::sync::Arc;

// ---------- archival-insert ----------

pub struct ArchivalInsertTool {
    semantic: Arc<SemanticStore>,
}

impl ArchivalInsertTool {
    pub fn new(semantic: Arc<SemanticStore>) -> Self {
        Self { semantic }
    }
}

#[async_trait]
impl AgentTool for ArchivalInsertTool {
    fn name(&self) -> &str {
        "archival-insert"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::InvalidInput(
                    "archival-insert requires 'content' field".to_string(),
                )
            })?;

        let key = payload
            .get("key")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                content
                    .split_whitespace()
                    .take(6)
                    .collect::<Vec<_>>()
                    .join(" ")
            });

        let tags: Vec<&str> = match payload.get("tags") {
            Some(serde_json::Value::Array(arr)) => {
                arr.iter().filter_map(|v| v.as_str()).collect()
            }
            Some(serde_json::Value::String(s)) => s
                .split(',')
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .collect(),
            _ => vec![],
        };

        let id = self
            .semantic
            .write(&key, content, Some(&context.agent_id), &tags)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "archival-insert".into(),
                reason: format!("Archival write failed: {}", e),
            })?;

        Ok(serde_json::json!({
            "success": true,
            "id": id,
            "message": "Content archived to long-term memory",
        }))
    }
}

// ---------- archival-search ----------

pub struct ArchivalSearchTool {
    semantic: Arc<SemanticStore>,
}

impl ArchivalSearchTool {
    pub fn new(semantic: Arc<SemanticStore>) -> Self {
        Self { semantic }
    }
}

#[async_trait]
impl AgentTool for ArchivalSearchTool {
    fn name(&self) -> &str {
        "archival-search"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::InvalidInput(
                    "archival-search requires 'query' field".to_string(),
                )
            })?;

        let top_k = payload
            .get("top_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let results = self
            .semantic
            .recall(query, Some(&context.agent_id), top_k)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "archival-search".into(),
                reason: format!("Archival search failed: {}", e),
            })?;

        let items: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "content": r.content,
                    "key": r.key,
                    "score": r.score,
                    "created_at": r.created_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "results": items,
            "count": items.len(),
        }))
    }
}
```

**Register the new modules in `crates/agentos-tools/src/lib.rs`:**

```rust
pub mod archival_tools;
pub mod memory_block_tools;

pub use archival_tools::{ArchivalInsertTool, ArchivalSearchTool};
pub use memory_block_tools::{
    MemoryBlockDeleteTool, MemoryBlockListTool, MemoryBlockReadTool, MemoryBlockWriteTool,
};
```

### 8.6 Register tools in `ToolRunner`

**File:** `crates/agentos-tools/src/runner.rs`

The `ToolRunner` must accept an `Arc<MemoryBlockStore>` to pass to the block tools. Update the constructor:

```rust
use crate::memory_block_tools::*;
use crate::archival_tools::*;
use agentos_kernel::memory_blocks::MemoryBlockStore;

impl ToolRunner {
    pub fn new_with_block_store(
        data_dir: &Path,
        model_cache_dir: &Path,
        block_store: Arc<MemoryBlockStore>,
    ) -> Self {
        let mut runner = Self::new_with_model_cache_dir(data_dir, model_cache_dir);

        runner.register(Box::new(MemoryBlockReadTool::new(block_store.clone())));
        runner.register(Box::new(MemoryBlockWriteTool::new(block_store.clone())));
        runner.register(Box::new(MemoryBlockListTool::new(block_store.clone())));
        runner.register(Box::new(MemoryBlockDeleteTool::new(block_store)));

        // Archival tools reuse the semantic store already in the runner.
        // These are wired in new_with_model_cache_dir where semantic is available.

        runner
    }
}
```

Alternatively, inject the `Arc<MemoryBlockStore>` via `Kernel::boot()` when constructing the `ToolRunner`, passing it alongside the existing `data_dir` and `model_cache_dir`.

### 8.7 Inject blocks into context

**File:** `crates/agentos-kernel/src/context.rs`

Add a method to `ContextManager` that injects memory blocks into a task's context window as a pinned System entry:

```rust
impl ContextManager {
    /// Inject the agent's memory blocks into the task's context window.
    ///
    /// Blocks are added as a pinned System entry with high importance (0.95)
    /// so they are never evicted. Called during context assembly before the
    /// first LLM inference for a task.
    pub async fn inject_memory_blocks(
        &self,
        task_id: &TaskID,
        blocks_text: &str,
    ) -> Result<(), AgentOSError> {
        if blocks_text.is_empty() {
            return Ok(());
        }

        let header = format!(
            "<memory_blocks>\n{}\n</memory_blocks>",
            blocks_text,
        );

        self.push_entry(
            task_id,
            ContextEntry {
                role: ContextRole::System,
                content: header,
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.95,
                pinned: true,
                reference_count: 0,
                partition: ContextPartition::Active,
            },
        )
        .await
    }
}
```

In the task executor (where context is assembled before calling the LLM), add:

```rust
// After create_context and system prompt injection, before first inference:
let blocks_text = kernel
    .memory_block_store
    .blocks_for_context(&agent_id)?;
kernel
    .context_manager
    .inject_memory_blocks(&task_id, &blocks_text)
    .await?;
```

### 8.8 Audit logging for block operations

Every block mutation should emit an audit entry. In the `MemoryBlockWriteTool` and `MemoryBlockDeleteTool` handlers, the kernel should log:

```rust
// After a successful write:
kernel.audit_log(agentos_audit::AuditEntry {
    timestamp: chrono::Utc::now(),
    trace_id: context.trace_id,
    event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
    agent_id: Some(context.agent_id),
    task_id: Some(context.task_id),
    tool_id: None,
    details: serde_json::json!({
        "tool": "memory-block-write",
        "label": label,
        "size": content.len(),
    }),
    severity: agentos_audit::AuditSeverity::Info,
    reversible: true,
    rollback_ref: None,
});
```

This uses the existing `ToolExecutionCompleted` event type. No new `AuditEventType` variants are needed.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/context.rs` | Add `MemoryBlock` struct with `validate_size()` |
| `crates/agentos-types/src/lib.rs` | Add `pub use context::MemoryBlock;` |
| `crates/agentos-kernel/src/memory_blocks.rs` | **New** -- `MemoryBlockStore` (SQLite-backed, `Mutex<Connection>`) |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod memory_blocks;` |
| `crates/agentos-kernel/src/kernel.rs` | Add `memory_block_store: Arc<MemoryBlockStore>` field; init in `boot()` |
| `crates/agentos-kernel/src/context.rs` | Add `inject_memory_blocks()` method to `ContextManager` |
| `crates/agentos-kernel/src/task_executor.rs` | Call `inject_memory_blocks()` during context assembly |
| `crates/agentos-tools/src/memory_block_tools.rs` | **New** -- 4 tool impls (`Read`, `Write`, `List`, `Delete`) |
| `crates/agentos-tools/src/archival_tools.rs` | **New** -- 2 tool impls (`Insert`, `Search`) |
| `crates/agentos-tools/src/lib.rs` | Register new modules and re-exports |
| `crates/agentos-tools/src/runner.rs` | Register block tools + archival tools with `ToolRunner` |
| `tools/core/memory-block-read.toml` | **New** manifest |
| `tools/core/memory-block-write.toml` | **New** manifest |
| `tools/core/memory-block-list.toml` | **New** manifest |
| `tools/core/memory-block-delete.toml` | **New** manifest |
| `tools/core/archival-insert.toml` | **New** manifest |
| `tools/core/archival-search.toml` | **New** manifest |

---

## Dependencies

- **Requires:**
  - Phase 3 ([[03-context-assembly-engine]]) -- `ContextManager` must support injecting blocks at prompt assembly time.
  - Phase 4 ([[04-procedural-memory-tier]]) -- archival tools delegate to `SemanticStore` for long-term persistence.
- **Blocks:** None -- this is a leaf phase.

---

## Test Plan

All tests use `tempfile::TempDir` for filesystem isolation and `MemoryBlockStore::open_in_memory()` for database isolation in pure-store tests. Tests that verify the full stack (tool + store) use `MemoryBlockStore::open(dir.path())`.

### Store-level tests

**File:** `crates/agentos-kernel/src/memory_blocks.rs` (inline `#[cfg(test)]` module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::AgentID;
    use tempfile::TempDir;

    #[test]
    fn test_write_and_read_block() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();

        let block = store.write(&agent, "persona", "I am a helpful coding assistant").unwrap();
        assert_eq!(block.label, "persona");
        assert_eq!(block.content, "I am a helpful coding assistant");
        assert_eq!(block.agent_id, agent);

        let retrieved = store.get(&agent, "persona").unwrap().unwrap();
        assert_eq!(retrieved.content, "I am a helpful coding assistant");
    }

    #[test]
    fn test_upsert_replaces_content() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();

        store.write(&agent, "notes", "version 1").unwrap();
        let updated = store.write(&agent, "notes", "version 2").unwrap();
        assert_eq!(updated.content, "version 2");

        let blocks = store.list(&agent).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].content, "version 2");
    }

    #[test]
    fn test_agent_scoping_enforced() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();

        store.write(&agent_a, "notes", "Agent A notes").unwrap();
        store.write(&agent_b, "notes", "Agent B notes").unwrap();

        let a_blocks = store.list(&agent_a).unwrap();
        assert_eq!(a_blocks.len(), 1);
        assert_eq!(a_blocks[0].content, "Agent A notes");

        let b_blocks = store.list(&agent_b).unwrap();
        assert_eq!(b_blocks.len(), 1);
        assert_eq!(b_blocks[0].content, "Agent B notes");

        // Agent A cannot see Agent B's block.
        assert!(store.get(&agent_a, "notes").unwrap().unwrap().content != "Agent B notes");
    }

    #[test]
    fn test_size_limit_enforced() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();
        let oversized = "x".repeat(MemoryBlock::MAX_SIZE + 1);

        let err = store.write(&agent, "big", &oversized).unwrap_err();
        assert!(matches!(err, AgentOSError::InvalidInput(_)));
    }

    #[test]
    fn test_exactly_max_size_accepted() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();
        let exact = "y".repeat(MemoryBlock::MAX_SIZE);

        let block = store.write(&agent, "exact", &exact).unwrap();
        assert_eq!(block.content.len(), MemoryBlock::MAX_SIZE);
    }

    #[test]
    fn test_empty_label_rejected() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();

        let err = store.write(&agent, "", "content").unwrap_err();
        assert!(matches!(err, AgentOSError::InvalidInput(_)));
    }

    #[test]
    fn test_delete_block() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();

        store.write(&agent, "temp", "temporary data").unwrap();
        assert!(store.get(&agent, "temp").unwrap().is_some());

        let deleted = store.delete(&agent, "temp").unwrap();
        assert!(deleted);
        assert!(store.get(&agent, "temp").unwrap().is_none());
    }

    #[test]
    fn test_delete_nonexistent_returns_false() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();

        let deleted = store.delete(&agent, "nonexistent").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn test_delete_does_not_affect_other_agents() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();

        store.write(&agent_a, "shared_label", "A's data").unwrap();
        store.write(&agent_b, "shared_label", "B's data").unwrap();

        store.delete(&agent_a, "shared_label").unwrap();

        // Agent B's block with the same label is untouched.
        let b_block = store.get(&agent_b, "shared_label").unwrap().unwrap();
        assert_eq!(b_block.content, "B's data");
    }

    #[test]
    fn test_blocks_for_context_formatting() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();

        store.write(&agent, "persona", "I am a coding assistant").unwrap();
        store.write(&agent, "preferences", "User likes concise answers").unwrap();

        let ctx = store.blocks_for_context(&agent).unwrap();
        assert!(ctx.contains("[persona]"));
        assert!(ctx.contains("I am a coding assistant"));
        assert!(ctx.contains("[preferences]"));
        assert!(ctx.contains("User likes concise answers"));
    }

    #[test]
    fn test_blocks_for_context_empty_agent() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();

        let ctx = store.blocks_for_context(&agent).unwrap();
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_count() {
        let store = MemoryBlockStore::open_in_memory().unwrap();
        let agent = AgentID::new();

        assert_eq!(store.count(&agent).unwrap(), 0);

        store.write(&agent, "a", "content").unwrap();
        store.write(&agent, "b", "content").unwrap();
        assert_eq!(store.count(&agent).unwrap(), 2);

        // Upsert does not change count.
        store.write(&agent, "a", "updated").unwrap();
        assert_eq!(store.count(&agent).unwrap(), 2);
    }

    #[test]
    fn test_sqlite_persistence_survives_reopen() {
        let dir = TempDir::new().unwrap();

        let agent = AgentID::new();

        // Write a block and drop the store.
        {
            let store = MemoryBlockStore::open(dir.path()).unwrap();
            store.write(&agent, "persisted", "this should survive").unwrap();
        }

        // Reopen from the same directory.
        {
            let store = MemoryBlockStore::open(dir.path()).unwrap();
            let block = store.get(&agent, "persisted").unwrap().unwrap();
            assert_eq!(block.content, "this should survive");
        }
    }
}
```

### Tool-level tests

**File:** `crates/agentos-tools/src/memory_block_tools.rs` (inline `#[cfg(test)]` module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agentos_kernel::memory_blocks::MemoryBlockStore;
    use agentos_types::*;
    use std::sync::Arc;

    fn make_ctx(agent_id: AgentID) -> ToolExecutionContext {
        let mut perms = PermissionSet::new();
        perms.grant("memory.blocks".to_string(), true, true, false, None);
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id,
            trace_id: TraceID::new(),
            permissions: perms,
            vault: None,
            hal: None,
        }
    }

    #[tokio::test]
    async fn test_write_then_read_via_tools() {
        let store = Arc::new(MemoryBlockStore::open_in_memory().unwrap());
        let agent = AgentID::new();

        let write_tool = MemoryBlockWriteTool::new(store.clone());
        let read_tool = MemoryBlockReadTool::new(store);

        let write_result = write_tool
            .execute(
                serde_json::json!({"label": "persona", "content": "I help with Rust code"}),
                make_ctx(agent),
            )
            .await
            .unwrap();
        assert_eq!(write_result["success"], true);
        assert_eq!(write_result["label"], "persona");

        let read_result = read_tool
            .execute(
                serde_json::json!({"label": "persona"}),
                make_ctx(agent),
            )
            .await
            .unwrap();
        assert_eq!(read_result["content"], "I help with Rust code");
    }

    #[tokio::test]
    async fn test_list_tool_returns_labels_and_sizes() {
        let store = Arc::new(MemoryBlockStore::open_in_memory().unwrap());
        let agent = AgentID::new();

        store.write(&agent, "a", "short").unwrap();
        store.write(&agent, "b", "a longer piece of content").unwrap();

        let list_tool = MemoryBlockListTool::new(store);
        let result = list_tool
            .execute(serde_json::json!({}), make_ctx(agent))
            .await
            .unwrap();
        assert_eq!(result["count"], 2);

        let blocks = result["blocks"].as_array().unwrap();
        assert!(blocks.iter().any(|b| b["label"] == "a" && b["size"] == 5));
        assert!(blocks.iter().any(|b| b["label"] == "b" && b["size"] == 25));
    }

    #[tokio::test]
    async fn test_delete_tool() {
        let store = Arc::new(MemoryBlockStore::open_in_memory().unwrap());
        let agent = AgentID::new();

        store.write(&agent, "temp", "data").unwrap();

        let delete_tool = MemoryBlockDeleteTool::new(store.clone());
        let result = delete_tool
            .execute(
                serde_json::json!({"label": "temp"}),
                make_ctx(agent),
            )
            .await
            .unwrap();
        assert_eq!(result["success"], true);

        assert!(store.get(&agent, "temp").unwrap().is_none());
    }

    #[tokio::test]
    async fn test_write_rejects_oversized_content() {
        let store = Arc::new(MemoryBlockStore::open_in_memory().unwrap());
        let agent = AgentID::new();
        let write_tool = MemoryBlockWriteTool::new(store);

        let oversized = "x".repeat(MemoryBlock::MAX_SIZE + 1);
        let err = write_tool
            .execute(
                serde_json::json!({"label": "big", "content": oversized}),
                make_ctx(agent),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AgentOSError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_write_requires_label() {
        let store = Arc::new(MemoryBlockStore::open_in_memory().unwrap());
        let agent = AgentID::new();
        let write_tool = MemoryBlockWriteTool::new(store);

        let err = write_tool
            .execute(
                serde_json::json!({"content": "no label given"}),
                make_ctx(agent),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AgentOSError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_read_nonexistent_returns_error_field() {
        let store = Arc::new(MemoryBlockStore::open_in_memory().unwrap());
        let agent = AgentID::new();
        let read_tool = MemoryBlockReadTool::new(store);

        let result = read_tool
            .execute(
                serde_json::json!({"label": "nonexistent"}),
                make_ctx(agent),
            )
            .await
            .unwrap();
        assert!(result["error"].is_string());
    }

    #[tokio::test]
    async fn test_cross_agent_isolation_via_tools() {
        let store = Arc::new(MemoryBlockStore::open_in_memory().unwrap());
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();

        let write_tool = MemoryBlockWriteTool::new(store.clone());
        let read_tool = MemoryBlockReadTool::new(store);

        // Agent A writes a block.
        write_tool
            .execute(
                serde_json::json!({"label": "secret", "content": "A's secret data"}),
                make_ctx(agent_a),
            )
            .await
            .unwrap();

        // Agent B tries to read the same label -- should get nothing.
        let result = read_tool
            .execute(
                serde_json::json!({"label": "secret"}),
                make_ctx(agent_b),
            )
            .await
            .unwrap();
        assert!(result["error"].is_string());
    }
}
```

---

## Verification

After implementing all subtasks, run these commands to confirm correctness:

```bash
# 1. Types compile
cargo build -p agentos-types

# 2. Memory block store compiles and passes tests
cargo test -p agentos-kernel -- memory_blocks

# 3. Tool implementations compile and pass tests
cargo test -p agentos-tools -- memory_block_tools
cargo test -p agentos-tools -- archival_tools

# 4. Full workspace builds cleanly
cargo build --workspace

# 5. All tests pass
cargo test --workspace

# 6. Lint passes
cargo clippy --workspace -- -D warnings

# 7. Verify tool manifests are loadable
cargo test -p agentos-kernel -- tool_registry

# 8. Verify SQLite persistence survives reopen
cargo test -p agentos-kernel -- test_sqlite_persistence_survives_reopen
```

---

## Related

- [[Memory Context Architecture Plan]] -- master plan for the 8-phase memory architecture
- [[Memory Context Research Synthesis]] -- Letta/MemGPT research backing
- [[Memory Context Data Flow]] -- how memory data flows through the system
- [[07-consolidation-pathways]] -- previous phase (memory tier consolidation)
- [[03-context-assembly-engine]] -- context compiler that injects blocks into prompts
- [[04-procedural-memory-tier]] -- procedural store that archival tools delegate to
