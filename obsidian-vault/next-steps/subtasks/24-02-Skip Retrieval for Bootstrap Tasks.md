---
title: Skip Retrieval for Bootstrap Tasks
tags:
  - kernel
  - memory
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: complete
effort: 4h
priority: critical
---

# Skip Retrieval for Bootstrap Tasks

> Update `execute_task_sync()` in `task_executor.rs` to consume the new `RetrievalOutcome` type and skip retrieval entirely for event-triggered bootstrap tasks.

---

## Why This Subtask

After subtask 24-01 changes `RetrievalExecutor::execute()` to return `RetrievalOutcome`, the caller in `task_executor.rs` (around line 367-408) must be updated. Additionally, event-triggered bootstrap tasks (those created by `AgentAdded` subscriptions at chain_depth > 0) should skip adaptive retrieval entirely because:

1. The agent was just registered -- it has zero episodic, semantic, or procedural memories.
2. Attempting retrieval is wasted work that produces a guaranteed `NoData` result.
3. The `MemorySearchFailed` event emission for empty results creates a noise cascade.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Retrieval call | Always runs if plan is non-empty | Skipped when `task.trigger_source.is_some()` (all event-triggered tasks) |
| `MemorySearchFailed` emission | On any empty retrieval result | Only on `RetrievalOutcome::SearchError` (actual search failures) |
| Error propagation | Silent | `SearchError` errors logged as `tracing::warn!` with search backend details |
| Retrieval result consumption | `let retrieved: Vec<RetrievalResult>` | `let outcome: RetrievalOutcome` then `outcome.into_results()` |

## What to Do

1. Open `crates/agentos-kernel/src/task_executor.rs`

2. In `execute_task_sync()`, locate the retrieval block (around line 364-408). Before the `if !retrieval_plan.is_empty()` check, add a condition to skip retrieval for bootstrap tasks:

```rust
// Skip adaptive retrieval for event-triggered bootstrap tasks.
// These tasks run immediately after AgentAdded events when the agent
// has no memories yet. Retrieval would return NoData and waste work.
let is_bootstrap_task = task.trigger_source.as_ref()
    .map(|ts| ts.chain_depth > 0)
    .unwrap_or(false);

if refresh_knowledge_blocks {
    let refresh_start = std::time::Instant::now();
    knowledge_blocks.clear();

    if !retrieval_plan.is_empty() && !is_bootstrap_task {
        let outcome = self
            .retrieval_executor
            .execute(&retrieval_plan, Some(&task.agent_id))
            .await;

        // Log actual search errors (not empty-store results)
        if outcome.has_errors() {
            for err in outcome.errors() {
                tracing::warn!(
                    task_id = %task.id,
                    error = %err,
                    "Retrieval backend error (results may be partial)"
                );
            }
            // Emit MemorySearchFailed only for actual infrastructure errors
            let chain_depth = task
                .trigger_source
                .as_ref()
                .map(|ts| ts.chain_depth + 1)
                .unwrap_or(0);
            self.emit_event_with_trace(
                EventType::MemorySearchFailed,
                EventSource::MemoryArbiter,
                EventSeverity::Warning,
                serde_json::json!({
                    "agent_id": task.agent_id.to_string(),
                    "task_id": task.id.to_string(),
                    "search_type": "adaptive_retrieval",
                    "query_count": retrieval_plan.queries.len(),
                    "errors": outcome.errors(),
                    "partial_results": !outcome.is_empty(),
                }),
                chain_depth,
                Some(iteration_trace_id),
            )
            .await;
        }

        let retrieved = outcome.into_results();
        knowledge_blocks =
            crate::retrieval_gate::RetrievalExecutor::format_as_knowledge_blocks(
                &retrieved,
            );
        tracing::debug!(
            task_id = %task.id,
            iteration,
            retrieval_queries = retrieval_plan.queries.len(),
            retrieval_results = retrieved.len(),
            retrieval_blocks = knowledge_blocks.len(),
            "Adaptive retrieval complete"
        );
    } else if is_bootstrap_task && !retrieval_plan.is_empty() {
        tracing::debug!(
            task_id = %task.id,
            "Skipping adaptive retrieval for bootstrap task (no memories exist yet)"
        );
    }

    // Memory blocks section unchanged
    if let Ok(blocks_context) = self.memory_blocks.blocks_for_context(&task.agent_id) {
        // ... existing code ...
    }
    // ... rest of refresh block unchanged ...
}
```

3. Remove the old `MemorySearchFailed` emission block (lines 384-408 in the original) -- it is replaced by the new `outcome.has_errors()` check above.

4. Add the import at the top of `task_executor.rs` if not already present:
```rust
use crate::retrieval_gate::RetrievalOutcome;
```

5. Verify the `trigger_source` field exists on `AgentTask`. Check `crates/agentos-types/src/task.rs`:
   - The field `trigger_source: Option<TriggerSource>` exists and contains `chain_depth`.
   - `chain_depth > 0` means this task was created in response to another event.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Update retrieval block to use `RetrievalOutcome`; skip retrieval for bootstrap tasks; emit `MemorySearchFailed` only on actual errors |

## Prerequisites

[[24-01-Retrieval Result Typing]] must be complete first -- this subtask depends on the `RetrievalOutcome` type.

## Test Plan

- `cargo build -p agentos-kernel` compiles cleanly (confirms both subtasks integrate)
- `cargo test -p agentos-kernel` -- all existing tests pass
- Add test `test_classify_task_failure_categories` confirming existing failure classification still works
- Confirm by code inspection: `MemorySearchFailed` is only emitted when `outcome.has_errors()` is true

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel --nocapture
cargo clippy -p agentos-kernel -- -D warnings
cargo fmt --all -- --check
```
