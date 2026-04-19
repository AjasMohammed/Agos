---
title: "Phase 1: Episodic Auto-Write on Task Completion"
tags:
  - plan
  - memory
  - episodic
  - kernel
  - v3
date: 2026-03-12
status: complete
effort: 1h
priority: critical
---

# Phase 1: Episodic Auto-Write on Task Completion

> Complete the episodic memory lifecycle — enrich existing records with structured metadata and fill remaining gaps (tool calls) so downstream phases (consolidation, retrieval) have full outcome data.

---

## Why This Phase

The codebase records user prompts, LLM responses, task failures, and — as of recent work — task successes and tool results. However, several gaps remain:

1. **Task success metadata is incomplete** — recorded inside `execute_task_sync()` but missing `duration_ms` and `iterations` (duration is only available in the outer `execute_task()`)
2. **Tool calls are never recorded** — we know what results came back, but not what was requested
3. **Tool result metadata is missing** — success/failure results are recorded but without structured metadata (tool name, success flag, iteration number)

Filling these gaps gives consolidation (Phase 7) full success/failure patterns with timing data, and gives retrieval (Phase 5) the ability to surface "what worked last time" with tool-level granularity.

---

## Current State

Already recorded in `execute_task_sync()` and `execute_task()`:

| Event | Type | Location | Metadata |
|-------|------|----------|----------|
| User prompt | `EpisodeType::UserPrompt` | `task_executor.rs:193-203` | — |
| LLM response | `EpisodeType::LLMResponse` | `task_executor.rs:675-688` | — |
| Task success | `EpisodeType::SystemEvent` | `task_executor.rs:1262-1284` | `outcome`, `tool_calls` |
| Task failure | `EpisodeType::SystemEvent` | `task_executor.rs:1337-1347` | `outcome`, `error` |
| Tool result (success) | `EpisodeType::ToolResult` | `task_executor.rs:1201-1211` | None (missing) |
| Tool result (failure) | `EpisodeType::ToolResult` | `task_executor.rs:1240-1249` | None (missing) |

**Remaining gaps:**

| Gap | Type | Why Needed |
|-----|------|-----------|
| Task success missing `duration_ms`, `iterations` | Metadata enrichment | Consolidation needs timing patterns; `duration_ms` only available in `execute_task()`, not `execute_task_sync()` |
| Tool call not recorded | `EpisodeType::ToolCall` | Track which tools were requested, in what order, with what intent |
| Tool result missing structured metadata | Metadata enrichment | Existing records pass `None` for metadata — need tool name, success flag, iteration |
| Pre-execution rejections not recorded | `EpisodeType::ToolResult` | Permission denied, coherence failure, security forbidden, awaiting approval (4 code paths) |

## Target State

- Task success recorded with full metadata: `outcome`, `duration_ms`, `tool_calls`, `iterations`
- Tool calls recorded with tool name, intent type, and iteration
- Tool results recorded with structured metadata (tool name, success/error, iteration)
- Pre-execution rejections recorded (stretch goal — can defer to Phase 2)
- All writes are non-blocking inline writes — they execute synchronously via `Mutex<Connection>` but never block the scheduler loop (each write runs inside an already-spawned task)

---

## Subtasks

### 1.1 ~~Record task success~~ (DONE — needs enrichment)

**Status:** Partially complete.

Task success is already recorded at `task_executor.rs:1262-1284` inside `execute_task_sync()`. The record includes `outcome: success` and `tool_calls` count.

**What's missing:** `duration_ms` and `iterations`. The `duration_ms` value is only available in `execute_task()` (the outer method that owns the `Instant::now()` timer), so the success record needs to move there. This requires subtask 1.2.

### 1.2 Return `TaskResult` from `execute_task_sync()`

**Status:** Not started.

**Where:** `crates/agentos-kernel/src/task_executor.rs`

**Why:** The canonical success write needs `duration_ms` (from `execute_task()`) and `iterations` + `tool_call_count` (from `execute_task_sync()`). A `TaskResult` struct bridges this.

Add the struct near the top of the `impl Kernel` block:

```rust
/// Result of synchronous task execution.
pub(crate) struct TaskResult {
    pub answer: String,
    pub tool_call_count: u32,
    pub iterations: u32,
}
```

Change `execute_task_sync()` signature:

```rust
// Before:
pub(crate) async fn execute_task_sync(
    &self,
    task: &AgentTask,
) -> Result<String, anyhow::Error> {

// After:
pub(crate) async fn execute_task_sync(
    &self,
    task: &AgentTask,
) -> Result<TaskResult, anyhow::Error> {
```

Update the return site at the end of `execute_task_sync()` (~line 1288):

```rust
// Before:
Ok(final_answer)

// After:
Ok(TaskResult {
    answer: final_answer,
    tool_call_count,
    iterations: iteration + 1, // iteration is 0-indexed; +1 for count
})
```

**Note on `iterations`:** The loop is `for iteration in 0..max_iterations`. At the `break` point, `iteration` holds the 0-based index of the last iteration. Use `iteration + 1` for a human-readable count. However, if the loop completes without breaking (all `max_iterations` exhausted), `iteration` is no longer in scope — handle this by tracking iterations with a separate `let mut iterations: u32 = 0;` counter incremented at the top of each loop body, or by capturing the value before the loop ends. The cleanest approach:

```rust
let mut completed_iterations: u32 = 0;
for iteration in 0..max_iterations {
    completed_iterations = iteration as u32 + 1;
    // ... existing loop body ...
}
```

Then return `iterations: completed_iterations` in the `TaskResult`.

**Remove the existing success write** at lines 1262-1284 in `execute_task_sync()` — this will be replaced by the enriched write in `execute_task()` (subtask 1.3).

### 1.3 Enrich success record in `execute_task()`

**Status:** Not started.

**Where:** `crates/agentos-kernel/src/task_executor.rs`, `execute_task()` method (~line 1292)

Update the `Ok(answer)` arm to use `TaskResult` and write the enriched episodic record:

```rust
match self.execute_task_sync(task).await {
    Ok(result) => {
        let duration_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            "Task {} complete: {}",
            task.id,
            &result.answer[..result.answer.len().min(100)]
        );
        crate::metrics::record_task_completed(duration_ms, true);

        // Record enriched task success to episodic memory
        if let Err(e) = self.episodic_memory.record(
            &task.id,
            &task.agent_id,
            agentos_memory::EpisodeType::SystemEvent,
            &format!(
                "Task completed: {}\nResult: {}",
                task.original_prompt,
                &result.answer[..result.answer.len().min(500)]
            ),
            Some("Task completed successfully"),
            Some(serde_json::json!({
                "outcome": "success",
                "duration_ms": duration_ms,
                "tool_calls": result.tool_call_count,
                "iterations": result.iterations,
            })),
            &agentos_types::TraceID::new(),
        ) {
            tracing::warn!(task_id = %task.id, error = %e, "Failed to record task completion");
        }

        self.scheduler
            .update_state(&task.id, TaskState::Complete)
            .await
            .ok();
        self.background_pool
            .complete(&task.id, serde_json::json!({ "result": result.answer }))
            .await;

        let waiters = self.scheduler.complete_dependency(task.id).await;
        for waiter_id in waiters {
            self.scheduler
                .update_state(&waiter_id, TaskState::Running)
                .await
                .ok();
        }
    }
    Err(e) => {
        // ... existing failure handling unchanged ...
    }
}
```

### 1.4 Record tool calls in the agent loop

**Status:** Not started.

**Where:** `crates/agentos-kernel/src/task_executor.rs`, after `self.intent_validator.record_tool_call()` (~line 768)

This is the one completely missing episode type. Add after `tool_call_count += 1;` (line 769):

```rust
// Record tool call to episodic memory
if let Err(e) = self.episodic_memory.record(
    &task.id,
    &task.agent_id,
    agentos_memory::EpisodeType::ToolCall,
    &serde_json::to_string(&tool_call.payload).unwrap_or_default(),
    Some(&format!("Called tool: {} ({:?})", tool_call.tool_name, tool_call.intent_type)),
    Some(serde_json::json!({
        "tool": tool_call.tool_name,
        "intent_type": format!("{:?}", tool_call.intent_type),
        "iteration": iteration,
    })),
    &trace_id,
) {
    tracing::warn!(task_id = %task.id, error = %e, "Failed to record tool call episode");
}
```

**Write volume note:** This fires once per tool call. A 10-iteration task with ~2 tool calls per iteration = ~20 writes. Combined with tool results, that's ~40 inline SQLite writes per task. The `EpisodicStore` uses `Mutex<Connection>`, so each write acquires the mutex and does a synchronous insert. This is acceptable for Phase 1 — each write is <1ms — but Phase 7 (consolidation) should consider batching if write volume becomes a bottleneck.

### 1.5 Enrich tool result metadata

**Status:** Not started (existing records need metadata enrichment).

**Where:** `crates/agentos-kernel/src/task_executor.rs`

Tool results are already recorded at two sites, but both pass `None` for metadata. Update them:

**Site A — Successful tool execution** (~line 1201-1211):

```rust
// Before:
if let Err(e) = self.episodic_memory.record(
    &task.id,
    &task.agent_id,
    agentos_memory::EpisodeType::ToolResult,
    &context_result.to_string(),
    Some(&format!("Tool '{}' succeeded", tool_call.tool_name)),
    None,
    &trace_id,
) {

// After:
if let Err(e) = self.episodic_memory.record(
    &task.id,
    &task.agent_id,
    agentos_memory::EpisodeType::ToolResult,
    &context_result.to_string(),
    Some(&format!("Tool '{}' succeeded", tool_call.tool_name)),
    Some(serde_json::json!({
        "tool": tool_call.tool_name,
        "success": true,
        "iteration": iteration,
    })),
    &trace_id,
) {
```

**Site B — Failed tool execution** (~line 1240-1249):

```rust
// Before:
if let Err(e) = self.episodic_memory.record(
    &task.id,
    &task.agent_id,
    agentos_memory::EpisodeType::ToolResult,
    &error_result.to_string(),
    Some(&format!("Tool '{}' failed: {}", tool_call.tool_name, e)),
    None,
    &trace_id,
) {

// After:
if let Err(e) = self.episodic_memory.record(
    &task.id,
    &task.agent_id,
    agentos_memory::EpisodeType::ToolResult,
    &error_result.to_string(),
    Some(&format!("Tool '{}' failed: {}", tool_call.tool_name, e)),
    Some(serde_json::json!({
        "tool": tool_call.tool_name,
        "success": false,
        "iteration": iteration,
        "error": e.to_string(),
    })),
    &trace_id,
) {
```

### 1.6 (Stretch) Record pre-execution rejections

**Status:** Deferred to Phase 2 unless time permits.

There are 4 `push_tool_result` sites for pre-execution rejections that are NOT recorded to episodic memory:

| Line | Reason | Current Recording |
|------|--------|-------------------|
| 735 | Permission denied | Audit log only |
| 751 | Coherence check failed | Audit log only |
| 846 | Security policy forbidden | Audit log only |
| 903 | Awaiting approval (HardApproval) | Audit log only |

These are already captured in the audit log with full details. Recording them to episodic memory would primarily benefit consolidation's ability to detect "this agent keeps getting blocked on X." This is lower priority than the core success/failure/tool-call gaps and can be added in a follow-up phase.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Add `TaskResult` struct; update `execute_task_sync()` return type; remove duplicate success write from `execute_task_sync()`; add enriched success write in `execute_task()`; add tool call recording; enrich tool result metadata at 2 existing sites |

---

## Verification

```bash
# Must compile cleanly
cargo build -p agentos-kernel

# All existing tests must still pass
cargo test --workspace

# Verify episodic entries are written (manual: run a task and check DB)
# sqlite3 data/episodic_memory.db "SELECT entry_type, summary, metadata FROM episodic_events ORDER BY timestamp DESC LIMIT 10"
```

**Expected entries after a single task with 2 tool calls:**

| entry_type | summary | metadata (key fields) |
|------------|---------|----------------------|
| `user_prompt` | User prompt text | — |
| `tool_call` | Called tool: file-reader (Read) | `tool`, `intent_type`, `iteration` |
| `tool_result` | Tool 'file-reader' succeeded | `tool`, `success: true`, `iteration` |
| `tool_call` | Called tool: shell-exec (Execute) | `tool`, `intent_type`, `iteration` |
| `tool_result` | Tool 'shell-exec' succeeded | `tool`, `success: true`, `iteration` |
| `llm_response` | LLM response text | — |
| `system_event` | Task completed successfully | `outcome: success`, `duration_ms`, `tool_calls: 2`, `iterations` |

---

## Dependencies

- **None** — this phase has no prerequisites
- **Blocks:** Phase 7 (consolidation needs success+failure patterns with timing), Phase 5 (retrieval benefits from complete episodic data with tool-level granularity)

## Related

- [[Memory Context Architecture Plan]] — master plan
- [[05-Episodic Memory Completion]] — original gap identification
- [[02-semantic-tool-discovery]] — deferred to V3.3+ (not needed until ~30+ tools)
