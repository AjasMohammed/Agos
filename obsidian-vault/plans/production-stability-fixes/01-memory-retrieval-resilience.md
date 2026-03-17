---
title: Memory Retrieval Resilience
tags:
  - kernel
  - memory
  - reliability
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 1.5d
priority: critical
---

# Phase 01 -- Memory Retrieval Resilience

> Fix the silent error swallowing in `RetrievalExecutor`, distinguish empty stores from broken searches, and skip retrieval for event-triggered bootstrap tasks.

---

## Why This Phase

Every event-triggered task fails with this pattern:

```
AgentAdded -> EventTriggeredTask -> TaskStarted -> MemorySearchFailed -> TaskRetrying -> TaskFailed
```

Root cause: `RetrievalExecutor::execute()` converts all search errors to empty vecs (`Err(_) => Vec::new()`), then `execute_task_sync()` emits `MemorySearchFailed` for any empty retrieval result. For a newly registered agent with zero memories, this is guaranteed to trigger, cascading into task failure.

The fix has two parts:
1. Make `RetrievalExecutor` return a typed result that distinguishes "no data found" from "search infrastructure error" and propagate actual errors as warnings (not failures).
2. Skip adaptive retrieval entirely for event-triggered bootstrap tasks (chain_depth > 0 with no prior episodic history), since the agent cannot possibly have memories yet.

## Sub-tasks

| # | Task | Files | Status | Detail Doc |
|---|------|-------|--------|------------|
| 01 | Typed retrieval results | `retrieval_gate.rs` | complete | [[24-01-Retrieval Result Typing]] |
| 02 | Skip retrieval for event-triggered tasks | `task_executor.rs` | complete | [[24-02-Skip Retrieval for Bootstrap Tasks]] |

## Test Plan

- Existing tests in `retrieval_gate.rs` continue to pass
- New test: `RetrievalExecutor::execute()` returns `RetrievalOutcome::NoData` when stores are empty (not an error)
- New test: `RetrievalExecutor::execute()` returns `RetrievalOutcome::SearchError` when a store returns `Err`
- Integration: an agent registration no longer triggers `MemorySearchFailed` events

## Verification

```bash
cargo test -p agentos-kernel -- retrieval
cargo test -p agentos-kernel -- task_executor
cargo clippy -p agentos-kernel -- -D warnings
```
