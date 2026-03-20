---
title: "Async Mutex Migration for Memory Stores"
tags:
  - next-steps
  - memory
  - performance
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 4h
priority: critical
---

# Async Mutex Migration for Memory Stores

> Wrap memory store database operations in `tokio::task::spawn_blocking()` to prevent blocking the async runtime under concurrent agent load.

## What to Do

All three memory stores (semantic, episodic, procedural) use `std::sync::Mutex` to protect SQLite connections. In an async runtime serving multiple agents:
1. Agent A writes to semantic memory → locks Mutex
2. Agent B searches semantic memory → **blocks the tokio worker thread**
3. All other async tasks on that thread are delayed

SQLite itself is not async-aware, so `tokio::sync::Mutex` alone won't help. The correct pattern is `spawn_blocking`.

### Steps

1. **For each memory store** (`semantic.rs`, `episodic.rs`, `procedural.rs`):
   - Keep `std::sync::Mutex` for the `Connection` (SQLite requires single-threaded access)
   - Wrap every public method's lock+query section in `tokio::task::spawn_blocking`:
     ```rust
     pub async fn search(&self, query: &str, ...) -> Result<Vec<...>> {
         let db = self.db.clone(); // Arc<Mutex<Connection>>
         let query = query.to_string();
         tokio::task::spawn_blocking(move || {
             let conn = db.lock().map_err(|_| ...)?;
             // ... existing SQLite query code ...
         }).await.map_err(|e| ...)?
     }
     ```

2. **Change method signatures from `&self` to `async`** where not already async

3. **Update all callers** — memory tool implementations that call these methods:
   - `memory_search.rs`, `memory_read.rs`, `memory_write.rs` (already async via `#[async_trait]`)
   - `episodic_list.rs`, `procedure_create.rs`, etc.
   - `task_executor.rs` (memory retrieval calls)

4. **Fix procedural store transaction pattern:**
   - `procedural.rs` uses manual `BEGIN/COMMIT TRANSACTION` instead of `conn.transaction()`
   - Migrate to `conn.transaction()` for safety (automatic ROLLBACK on error)

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-memory/src/semantic.rs` | Wrap all methods in `spawn_blocking` |
| `crates/agentos-memory/src/episodic.rs` | Wrap all methods in `spawn_blocking` |
| `crates/agentos-memory/src/procedural.rs` | Wrap all methods in `spawn_blocking`, fix transaction pattern |
| Tool files in `crates/agentos-tools/src/` | Update callers if signatures changed |

## Prerequisites

None — independent.

## Verification

```bash
cargo test -p agentos-memory
cargo test -p agentos-tools
cargo clippy --workspace -- -D warnings
```

Test: concurrent memory operations from multiple agents don't block each other (use `tokio::test` with multiple spawned tasks writing/reading simultaneously).
