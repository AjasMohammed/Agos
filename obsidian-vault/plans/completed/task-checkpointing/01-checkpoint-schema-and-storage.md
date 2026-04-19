---
title: "Phase 1: Checkpoint Schema and Storage"
tags:
  - kernel
  - durability
  - plan
date: 2026-04-03
status: complete
effort: 1d
priority: high
---

# Phase 1: Checkpoint Schema and Storage

> Create the SQLite `checkpoints` table and `CheckpointStore` struct that provides async read/write access to checkpoint data, following the same `spawn_blocking` + `Mutex<Connection>` pattern used by `UserInbox` and `AuditLog`.

---

## Why This Phase

All subsequent phases (serialization, recovery, lifecycle) depend on a persistent store for checkpoint data. This phase creates the storage layer in isolation so it can be tested independently before wiring it into the task executor.

The pattern follows `crates/agentos-kernel/src/user_inbox.rs` exactly: a `rusqlite::Connection` behind `Arc<Mutex<Connection>>`, with all I/O on `tokio::task::spawn_blocking` to avoid blocking the async runtime.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Checkpoint persistence | None | SQLite table `checkpoints` in `data/checkpoints.db` |
| `CheckpointStore` type | Does not exist | New struct in `crates/agentos-kernel/src/checkpoint_store.rs` |
| Checkpoint record | Does not exist | `CheckpointRecord` struct with task_id, context_blob, metadata |

## What to Do

### 1. Create the checkpoint store module

Create `crates/agentos-kernel/src/checkpoint_store.rs`:

```rust
use agentos_types::{AgentID, AgentOSError, TaskID};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Unique identifier for a checkpoint.
pub type CheckpointID = Uuid;

/// A single checkpoint record, representing the state of a task at a specific step.
#[derive(Debug, Clone)]
pub struct CheckpointRecord {
    pub id: CheckpointID,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub step_num: u32,
    pub created_at: DateTime<Utc>,
    /// Encrypted JSON blob of the ContextWindow.
    pub context_blob: Vec<u8>,
    /// JSON-serialized AgentTask metadata (state, prompt, history pointers).
    pub task_state_json: String,
    /// JSON-serialized tool call history for replay.
    pub tool_call_history_json: String,
}

/// SQLite-backed persistent store for task checkpoints.
///
/// Follows the same async pattern as `UserInbox`: `rusqlite::Connection` behind
/// `Arc<Mutex<Connection>>`, all I/O on `spawn_blocking`.
pub struct CheckpointStore {
    db: Arc<Mutex<Connection>>,
}

impl CheckpointStore {
    /// Open (or create) the checkpoint database at `db_path`.
    pub fn new(db_path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path).map_err(|e| AgentOSError::KernelError {
            reason: format!("CheckpointStore: failed to open DB at {}: {}", db_path.display(), e),
        })?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("CheckpointStore: PRAGMA failed: {e}"),
            })?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS checkpoints (
                id                      TEXT PRIMARY KEY,
                task_id                 TEXT NOT NULL,
                agent_id                TEXT NOT NULL,
                step_num                INTEGER NOT NULL,
                created_at              TEXT NOT NULL,
                context_blob            BLOB NOT NULL,
                task_state_json         TEXT NOT NULL,
                tool_call_history_json  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cp_task_id ON checkpoints(task_id);
            CREATE INDEX IF NOT EXISTS idx_cp_created_at ON checkpoints(created_at);",
        )
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("CheckpointStore: schema creation failed: {e}"),
        })?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
        })
    }

    /// Write a checkpoint, replacing any existing checkpoint for the same task_id.
    /// Only the latest checkpoint per task is kept (upsert on task_id).
    pub async fn write(&self, record: &CheckpointRecord) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let record = record.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            // Delete previous checkpoint for this task (keep only latest).
            conn.execute(
                "DELETE FROM checkpoints WHERE task_id = ?1",
                params![record.task_id.to_string()],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("CheckpointStore: delete old checkpoint failed: {e}"),
            })?;

            conn.execute(
                "INSERT INTO checkpoints
                 (id, task_id, agent_id, step_num, created_at,
                  context_blob, task_state_json, tool_call_history_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.id.to_string(),
                    record.task_id.to_string(),
                    record.agent_id.to_string(),
                    record.step_num as i64,
                    record.created_at.to_rfc3339(),
                    record.context_blob,
                    record.task_state_json,
                    record.tool_call_history_json,
                ],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("CheckpointStore: insert failed: {e}"),
            })?;
            Ok::<_, AgentOSError>(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("CheckpointStore: task join error: {e}"),
        })??;
        Ok(())
    }

    /// Retrieve the latest checkpoint for a task.
    pub async fn get_latest(&self, task_id: &TaskID) -> Result<Option<CheckpointRecord>, AgentOSError> {
        let db = self.db.clone();
        let task_id_str = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let result = conn.query_row(
                "SELECT id, task_id, agent_id, step_num, created_at,
                        context_blob, task_state_json, tool_call_history_json
                 FROM checkpoints WHERE task_id = ?1
                 ORDER BY created_at DESC LIMIT 1",
                params![task_id_str],
                |row| {
                    Ok(CheckpointRecord {
                        id: row.get::<_, String>(0)?.parse().unwrap_or_default(),
                        task_id: row.get::<_, String>(1)?.parse().unwrap_or_default(),
                        agent_id: row.get::<_, String>(2)?.parse().unwrap_or_default(),
                        step_num: row.get::<_, i64>(3)? as u32,
                        created_at: chrono::DateTime::parse_from_rfc3339(
                            &row.get::<_, String>(4)?
                        )
                        .unwrap_or_default()
                        .with_timezone(&Utc),
                        context_blob: row.get(5)?,
                        task_state_json: row.get(6)?,
                        tool_call_history_json: row.get(7)?,
                    })
                },
            );
            match result {
                Ok(record) => Ok(Some(record)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(AgentOSError::KernelError {
                    reason: format!("CheckpointStore: get_latest failed: {e}"),
                }),
            }
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("CheckpointStore: task join error: {e}"),
        })?
    }

    /// List all task IDs that have checkpoints (for recovery on boot).
    pub async fn list_checkpointed_tasks(&self) -> Result<Vec<TaskID>, AgentOSError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let mut stmt = conn.prepare(
                "SELECT DISTINCT task_id FROM checkpoints ORDER BY created_at DESC"
            ).map_err(|e| AgentOSError::KernelError {
                reason: format!("CheckpointStore: prepare failed: {e}"),
            })?;
            let rows = stmt.query_map([], |row| {
                let id_str: String = row.get(0)?;
                Ok(id_str)
            }).map_err(|e| AgentOSError::KernelError {
                reason: format!("CheckpointStore: query failed: {e}"),
            })?;
            let mut ids = Vec::new();
            for row in rows {
                if let Ok(id_str) = row {
                    if let Ok(id) = id_str.parse() {
                        ids.push(id);
                    }
                }
            }
            Ok(ids)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("CheckpointStore: task join error: {e}"),
        })?
    }

    /// Delete all checkpoints older than `max_age`.
    pub async fn prune_older_than(&self, max_age: chrono::Duration) -> Result<usize, AgentOSError> {
        let db = self.db.clone();
        let cutoff = (Utc::now() - max_age).to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            let deleted = conn.execute(
                "DELETE FROM checkpoints WHERE created_at < ?1",
                params![cutoff],
            ).map_err(|e| AgentOSError::KernelError {
                reason: format!("CheckpointStore: prune failed: {e}"),
            })?;
            Ok(deleted)
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("CheckpointStore: task join error: {e}"),
        })?
    }

    /// Delete checkpoint for a specific task (on normal completion).
    pub async fn delete_for_task(&self, task_id: &TaskID) -> Result<(), AgentOSError> {
        let db = self.db.clone();
        let task_id_str = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            conn.execute(
                "DELETE FROM checkpoints WHERE task_id = ?1",
                params![task_id_str],
            )
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("CheckpointStore: delete_for_task failed: {e}"),
            })?;
            Ok::<_, AgentOSError>(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("CheckpointStore: task join error: {e}"),
        })??;
        Ok(())
    }
}
```

### 2. Register the module

Open `crates/agentos-kernel/src/lib.rs`. Add:

```rust
pub mod checkpoint_store;
```

### 3. Add `checkpoint_store` to `Kernel` struct

Open `crates/agentos-kernel/src/kernel.rs`. Add field:

```rust
pub(crate) checkpoint_store: Option<Arc<CheckpointStore>>,
```

Initialize in the constructor (only when config enables checkpointing):

```rust
let checkpoint_store = if config.kernel.checkpointing_enabled {
    let cp_path = data_dir.join("checkpoints.db");
    Some(Arc::new(CheckpointStore::new(&cp_path)?))
} else {
    None
};
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/checkpoint_store.rs` | New file: `CheckpointStore` struct, `CheckpointRecord` type, CRUD methods |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod checkpoint_store;` |
| `crates/agentos-kernel/src/kernel.rs` | Add `checkpoint_store: Option<Arc<CheckpointStore>>` field; initialize in constructor |

## Prerequisites

None -- this is the first phase.

## Test Plan

- **Unit test `test_checkpoint_write_and_read`:** Create a `CheckpointStore` with `tempfile`, write a `CheckpointRecord`, read it back with `get_latest`, assert all fields match.
- **Unit test `test_checkpoint_upsert_replaces_previous`:** Write two checkpoints for the same `task_id` with different `step_num`. Call `get_latest`. Assert only the second checkpoint is returned.
- **Unit test `test_list_checkpointed_tasks`:** Write checkpoints for 3 different tasks. Call `list_checkpointed_tasks`. Assert 3 task IDs returned.
- **Unit test `test_prune_older_than`:** Write a checkpoint with `created_at` set to 73 hours ago. Call `prune_older_than(72h)`. Assert the checkpoint is deleted. Write another with `created_at = now`. Assert it survives pruning.
- **Unit test `test_delete_for_task`:** Write a checkpoint, call `delete_for_task`, assert `get_latest` returns `None`.
- **Unit test `test_empty_store`:** Call `get_latest` on an empty store. Assert `None` returned without error.

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- checkpoint --nocapture
cargo clippy -p agentos-kernel -- -D warnings
cargo fmt --all -- --check
```
