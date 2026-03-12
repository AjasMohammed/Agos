---
title: Episodic Memory — Auto-Write on Task Completion
tags:
  - next-steps
  - memory
  - phase-5
date: 2026-03-11
status: done
effort: 1h
priority: high
feedback-plan-ref: "Phase 5.1"
---

# Episodic Memory — Auto-Write on Task Completion

> Closes the last gap in Phase 5 of the Feedback Implementation Plan.

---

## Current State

**Phase 5.2 (auto-inject) is done** — at task start, `task_executor.rs` queries episodic memory for relevant past episodes and injects a `[EPISODIC_RECALL]` block into the context.

**Phase 5.1 (auto-write) is NOT done** — when a task completes, no summary is written to the episodic store. This means agents learn from past runs only if something else writes to episodic memory, which currently only happens via explicit tool calls.

---

## The Gap

```
Task starts
    ↓
[EPISODIC_RECALL] injected ← ✅ DONE
    ↓
Agent runs...
    ↓
Task completes
    ↓
Episodic summary written ← ❌ MISSING
```

Without the write-on-completion step, the recall at the *next* task has nothing new to recall. The system has short-term memory but doesn't build long-term episodic experience.

---

## What to Implement

### Location

**File:** `crates/agentos-kernel/src/task_executor.rs`

In `execute_task_sync()`, find the block after `TaskState::Complete` is set and the final answer is assembled. This is where the episodic write belongs.

> [!tip] Future home
> When [[04-Kernel Modularization]] is done, this logic moves to `task_completion.rs::TaskCompletionHandler::on_success()`. For now, add it directly to `task_executor.rs`.

### Code to Add

```rust
// After: task state set to Complete, `answer` string is available
// Before: returning the answer

let episode_summary = format!(
    "Task: {}\nOutcome: Success\nTool calls made: {}\nFinal answer preview: {}",
    task.original_prompt,
    tool_call_count,  // already tracked in the execution loop
    &answer[..answer.len().min(500)]
);

let _ = self.episodic_memory
    .record(
        &task.id,
        &task.agent_id,
        agentos_memory::EpisodeType::TaskOutcome,
        &episode_summary,
        Some("Task completed successfully"),
        Some(serde_json::json!({
            "outcome": "success",
            "tool_calls": tool_call_count,
            "task_prompt_preview": &task.original_prompt[..task.original_prompt.len().min(200)],
        })),
        &agentos_types::TraceID::new(),
    )
    .await;
// Use `.ok()` or `let _ =` — failure to write episodic memory
// should never fail the task itself
```

Also add failure recording in the error path:

```rust
// In the task failure branch:
let _ = self.episodic_memory
    .record(
        &task.id,
        &task.agent_id,
        agentos_memory::EpisodeType::TaskOutcome,
        &format!("Task failed: {}\nError: {}", task.original_prompt, error_message),
        Some("Task failed"),
        Some(serde_json::json!({ "outcome": "failure", "error": error_message })),
        &agentos_types::TraceID::new(),
    )
    .await;
```

---

## Why `EpisodeType::TaskOutcome`

Check the existing `EpisodeType` variants in `crates/agentos-memory/src/episodic.rs`. If `TaskOutcome` doesn't exist, use `SystemEvent` (which already exists) or add the variant:

```rust
// If adding a new variant:
pub enum EpisodeType {
    // ... existing variants ...
    TaskOutcome,  // Written when a task completes or fails
}
```

---

## Testing

Add to the existing episodic memory test suite or to `task_executor` tests:

```rust
#[tokio::test]
async fn test_episodic_write_on_task_completion() {
    // 1. Create kernel with MockLLM that returns a fixed answer
    // 2. Run a task via execute_task_sync()
    // 3. Search episodic memory for the task prompt
    // 4. Assert: at least 1 episode found with outcome: "success"
}
```

---

## Impact

Once this is in place, a second run of any similar task will automatically receive relevant context from the first run. This is the foundation for agent learning across sessions.

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/task_executor.rs` | Add episodic write in success + failure paths |
| `crates/agentos-memory/src/episodic.rs` | Add `EpisodeType::TaskOutcome` variant if missing |

---

## Related

- [[04-Kernel Modularization]] — Where this logic will eventually live
- [[Index]] — Back to dashboard
