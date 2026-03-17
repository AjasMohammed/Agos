---
title: "Phase 05 — Memory Event Emission & Prompt"
tags:
  - kernel
  - event-system
  - memory
  - plan
  - v3
date: 2026-03-13
status: complete
effort: 3h
priority: high
---
# Phase 05 — Memory Event Emission & Prompt

> Wire ContextWindowNearLimit, EpisodicMemoryWritten, and SemanticMemoryConflict events, plus a custom trigger prompt for context pressure.

---

## Why This Phase

Memory events are critical for agent self-management. When context windows fill up, agents need to proactively archive important information to episodic memory before blind eviction destroys it. The `ContextWindowNearLimit` event is specifically designed to give agents a chance to manage their own context — spec §7.4 defines a detailed prompt for this.

---

## Current State

| What | Status |
|------|--------|
| `EventType::ContextWindowNearLimit` / `EpisodicMemoryWritten` / `SemanticMemoryConflict` | Defined in `agentos-types/src/event.rs` |
| `context_compiler.rs` compiles context and tracks token counts | Working — produces compiled context with token estimates |
| `episodic.rs` records episodes | Working — `record()` method stores entries |
| `semantic.rs` writes memories | Working — handles key conflicts |
| **Event emission in any of these** | **None** |
| Memory subsystems have no access to `event_sender` | Needs injection or caller-side emission |

---

## Target State

- `ContextWindowNearLimit` emitted from `task_executor.rs` after context compilation, when token usage exceeds 80% of budget
- `EpisodicMemoryWritten` emitted from `task_executor.rs` after successful episodic memory record
- `SemanticMemoryConflict` emitted from `task_executor.rs` or memory tool when a write detects a conflict
- Custom trigger prompt for `ContextWindowNearLimit` with memory management guidance

---

## Subtasks

### 1. Emit `ContextWindowNearLimit` from `task_executor.rs`

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** After calling the context compiler and receiving the compiled context back. The compiled result should include token count information. Check if `estimated_tokens / max_tokens > 0.80`.

**Code:**

```rust
// After context compilation:
let utilization = estimated_tokens as f32 / max_tokens as f32;
if utilization > 0.80 {
    self.emit_event(
        EventType::ContextWindowNearLimit,
        EventSource::ContextManager,
        if utilization > 0.95 { EventSeverity::Critical } else { EventSeverity::Warning },
        serde_json::json!({
            "task_id": task.id.to_string(),
            "agent_id": task.agent_id.to_string(),
            "estimated_tokens": estimated_tokens,
            "max_tokens": max_tokens,
            "utilization_percent": (utilization * 100.0) as u32,
        }),
        0,
    ).await;
}
```

**Important:** This must fire at most once per task (spec says `max_once_per:task` throttle). The simplest approach: track a `bool context_warning_emitted` in the task loop and only emit once.

### 2. Emit `EpisodicMemoryWritten` from `task_executor.rs`

**File:** `crates/agentos-kernel/src/task_executor.rs`

**Where:** After episodic memory is written (e.g., after task completion writes an episode, or when the `memory-write` tool successfully records an episode).

```rust
self.emit_event(
    EventType::EpisodicMemoryWritten,
    EventSource::MemoryArbiter,
    EventSeverity::Info,
    serde_json::json!({
        "task_id": task.id.to_string(),
        "agent_id": task.agent_id.to_string(),
        "entry_type": "task_completion",
        "summary": summary_preview.chars().take(200).collect::<String>(),
    }),
    0,
).await;
```

### 3. Emit `SemanticMemoryConflict` from memory tool execution

**File:** `crates/agentos-kernel/src/task_executor.rs` (or the memory-write tool handler)

**Where:** When `semantic.write()` or equivalent detects a conflict (duplicate key, version mismatch). The memory module itself shouldn't depend on event infrastructure — instead, check the return value from the memory write and emit from the caller.

```rust
// After memory write returns a conflict indicator:
if memory_write_result.had_conflict {
    self.emit_event(
        EventType::SemanticMemoryConflict,
        EventSource::MemoryArbiter,
        EventSeverity::Warning,
        serde_json::json!({
            "agent_id": task.agent_id.to_string(),
            "key": memory_key,
            "conflict_type": "duplicate_key",
        }),
        0,
    ).await;
}
```

### 4. Add `build_context_window_near_limit_prompt()` to `trigger_prompt.rs`

**File:** `crates/agentos-kernel/src/trigger_prompt.rs`

**Prompt structure (from spec §7.4):**

```rust
async fn build_context_window_near_limit_prompt(
    &self,
    event: &EventMessage,
    subscriber_agent_id: &AgentID,
) -> String {
    let task_id = event.payload["task_id"].as_str().unwrap_or("unknown");
    let estimated_tokens = event.payload["estimated_tokens"].as_u64().unwrap_or(0);
    let max_tokens = event.payload["max_tokens"].as_u64().unwrap_or(0);
    let utilization = event.payload["utilization_percent"].as_u64().unwrap_or(0);

    let agent_info = self.get_agent_info_for_prompt(subscriber_agent_id).await;

    format!(
        "[SYSTEM CONTEXT]\n\
         You are {agent_name} currently executing task {task_id}.\n\n\
         [EVENT NOTIFICATION]\n\
         Your context window is approaching its limit.\n\n\
         Current usage: {estimated_tokens} / {max_tokens} tokens ({utilization}%)\n\
         Estimated remaining capacity: ~{remaining} tokens\n\n\
         [AVAILABLE ACTIONS]\n\
         You may:\n\
           - Use memory-write to archive important context to episodic memory\n\
           - Request a context checkpoint\n\
           - Explicitly flag entries as important to protect them from eviction\n\
           - Continue without action (kernel will auto-evict if needed)\n\n\
         [GUIDANCE]\n\
         Consider: Are there tool results from earlier in this task that you no\n\
         longer need in active context but should preserve in episodic memory?\n\
         Now is the time to write them. Do not wait until the window is full.\n\n\
         [RESPONSE EXPECTATION]\n\
         Take any context management actions you deem necessary, then continue."
    )
}
```

Add a match arm in `build_trigger_prompt()`:

```rust
EventType::ContextWindowNearLimit => {
    self.build_context_window_near_limit_prompt(event, subscriber_agent_id).await
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Add 3 emission calls: ContextWindowNearLimit, EpisodicMemoryWritten, SemanticMemoryConflict |
| `crates/agentos-kernel/src/trigger_prompt.rs` | Add `build_context_window_near_limit_prompt()` + match arm |

---

## Dependencies

None — can be done in parallel with Phases 01, 02, 04, 06.

---

## Test Plan

1. **Context threshold test:** Mock a context compilation that returns 85% utilization, verify `ContextWindowNearLimit` event is emitted.

2. **Below-threshold test:** Mock 70% utilization, verify no event.

3. **Once-per-task test:** Verify the event fires at most once per task execution even if utilization remains above 80%.

4. **Prompt test:** Construct a mock event with known token counts, call `build_trigger_prompt()`, verify output includes the token numbers and management guidance.

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel

grep -n "ContextWindowNearLimit" crates/agentos-kernel/src/task_executor.rs
grep -n "EpisodicMemoryWritten" crates/agentos-kernel/src/task_executor.rs
grep -n "context_window_near_limit_prompt" crates/agentos-kernel/src/trigger_prompt.rs
```

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[agentos-event-trigger-system]] — Original spec §7.4 (ContextWindowNearLimit prompt)
- [[Memory Context Architecture Plan]] — Memory subsystem architecture
