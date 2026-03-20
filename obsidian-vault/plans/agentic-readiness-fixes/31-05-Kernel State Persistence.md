---
title: "Kernel State Persistence to SQLite"
tags:
  - next-steps
  - kernel
  - persistence
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 2d
priority: critical
---

# Kernel State Persistence to SQLite

> Persist scheduler queue, escalation state, and cost snapshots to SQLite so kernel restarts don't lose all in-progress work.

## Problem

All kernel state (tasks, escalations, cost snapshots) was in-memory. A kernel restart meant:
- All in-progress tasks lost
- All pending escalation approvals lost
- All cost tracking reset to zero
- Long-running autonomous tasks could not survive restarts

## Implementation

### Core module: `crates/agentos-kernel/src/state_store.rs`

`KernelStateStore` wraps a single `rusqlite::Connection` behind `Arc<Mutex<Connection>>`. All public methods are `async` and execute blocking SQLite I/O through `tokio::task::spawn_blocking`.

#### Connection configuration

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
```

WAL mode allows concurrent readers while a writer holds the lock. `busy_timeout` prevents immediate failures under contention.

#### Migration system

A `kernel_state_migrations` table tracks applied versions. Migrations are sequential DDL arrays — adding a new migration is appending to the array with the next version number. On boot, the store applies any unapplied migrations and validates the final version matches `LATEST_MIGRATION_VERSION` (currently 1). If validation fails, kernel boot aborts.

### Schema (Migration v1)

**`scheduler_tasks`**
| Column | Type | Notes |
|--------|------|-------|
| `task_id` | TEXT PRIMARY KEY | UUID string |
| `agent_id` | TEXT NOT NULL | |
| `state` | TEXT NOT NULL | queued/running/waiting/complete/failed/cancelled |
| `priority` | INTEGER NOT NULL | |
| `enqueued_at` | TEXT NOT NULL | RFC 3339 |
| `payload` | BLOB NOT NULL | Full JSON-serialized `AgentTask` |
| `updated_at` | TEXT NOT NULL | RFC 3339 |

Indexes: `state`, `(priority DESC, enqueued_at ASC)`

**`pending_escalations`**
| Column | Type | Notes |
|--------|------|-------|
| `escalation_id` | TEXT PRIMARY KEY | |
| `task_id` | TEXT NOT NULL | |
| `agent_id` | TEXT NOT NULL | |
| `risk_level` | TEXT NOT NULL | |
| `description` | TEXT NOT NULL | |
| `created_at` | TEXT NOT NULL | RFC 3339 |
| `expires_at` | TEXT NOT NULL | RFC 3339 |
| `resolved` | INTEGER NOT NULL DEFAULT 0 | |
| `payload` | BLOB NOT NULL | Full JSON-serialized `PendingEscalation` |
| `resolution` | TEXT | Nullable — set on resolve |
| `resolved_at` | TEXT | Nullable — set on resolve |

Indexes: `resolved`, `expires_at`

**`cost_snapshots`**
| Column | Type | Notes |
|--------|------|-------|
| `agent_id` | TEXT PRIMARY KEY | |
| `agent_name` | TEXT NOT NULL DEFAULT '' | |
| `input_tokens` | INTEGER NOT NULL DEFAULT 0 | |
| `output_tokens` | INTEGER NOT NULL DEFAULT 0 | |
| `total_cost_usd` | REAL NOT NULL DEFAULT 0.0 | |
| `tool_calls` | INTEGER NOT NULL DEFAULT 0 | |
| `period_start` | TEXT NOT NULL | RFC 3339 |
| `version` | INTEGER NOT NULL DEFAULT 0 | Monotonic — upsert uses `WHERE excluded.version >= cost_snapshots.version` to prevent out-of-order writes |

### Integration points

**Scheduler** (`scheduler.rs`):
- `persist_task_snapshot()` called on enqueue, state transitions, complete, fail, cancel
- `restore_from_store()` reloads non-terminal tasks; Running tasks are normalized to Queued

**Escalation Manager** (`escalation.rs`):
- `persist_escalation()` called on create, resolve, sweep
- `restore_from_store()` reloads unresolved escalations; next ID continues past persisted rows
- Sweep logic runs in Rust, persists per-row afterward (no bulk SQL UPDATE)

**Cost Tracker** (`cost_tracker.rs`):
- `persist_snapshot()` called from `record_inference()` and `record_tool_call()`
- `restore_from_store()` reloads all snapshots; survives unregister/re-register cycles

**Kernel boot** (`kernel.rs`):
- `KernelStateStore::open()` creates parent dirs, opens DB, runs migrations
- Passes `Arc<KernelStateStore>` to scheduler, escalation manager, cost tracker
- Calls `restore_from_store()` on all three; logs restored counts

### Error handling strategy

- **Boot failures are fatal** — if `open()` or `restore_from_store()` fails, kernel aborts
- **Runtime persistence failures are logged** — `persist_*()` methods log `tracing::error!` but do NOT propagate errors to callers, so in-memory state remains authoritative

### Config

`state_db_path` in `[kernel]` section of `config/default.toml`, default `data/kernel_state.db`. Overridable via `AGENTOS_STATE_DB_PATH` env var.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/state_store.rs` | **New** — 679-line `KernelStateStore` with open/upsert/load/migrate |
| `crates/agentos-kernel/src/lib.rs` | Added `pub mod state_store;` |
| `crates/agentos-kernel/src/kernel.rs` | State DB initialization, passes to subsystems, calls `restore_from_store()` |
| `crates/agentos-kernel/src/scheduler.rs` | `state_store` field, `persist_task_snapshot()`, `restore_from_store()` |
| `crates/agentos-kernel/src/escalation.rs` | `state_store` field, `persist_escalation()`, `restore_from_store()` |
| `crates/agentos-kernel/src/cost_tracker.rs` | `state_store` field, `persist_snapshot()`, `restore_from_store()` |
| `crates/agentos-kernel/src/config.rs` | Added `state_db_path` with default and env override |
| `config/default.toml` | Added `state_db_path` |

## Verification

```bash
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Tests cover:
- Enqueue task → restore → task is re-enqueued (Running normalized to Queued)
- Waiting tasks remain paused after restore
- Create escalation → restore → escalation still pending
- Resolved escalations are NOT restored
- Next escalation ID continues past persisted rows
- Record cost → restore → snapshot preserved
- Cost snapshots survive unregister/re-register cycles

## Related

[[Agentic Readiness Fixes Plan]]
