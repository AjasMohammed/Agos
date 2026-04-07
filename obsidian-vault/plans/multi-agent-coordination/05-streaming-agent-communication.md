---
title: "Phase 5: Streaming Agent Communication"
tags:
  - kernel
  - agents
  - v4
  - plan
date: 2026-04-07
status: partial
effort: 2d
priority: high
---

# Phase 5: Streaming Agent Communication

> Enable parent agents to receive progressive status updates from spawned child agents, allowing early intervention, partial result incorporation, and long-running task monitoring without blocking.

---

## Why This Phase

Phases 1-4 established a request/response model for multi-agent coordination: `spawn-agent` fires, the parent either blocks on `await-agents` or continues blind. For short tasks this is fine. For long-running subtasks (research, multi-step pipelines, complex analysis), it creates two problems:

1. **No course-correction** — if a child agent goes off-track, the parent only discovers this after the child completes (wasting tokens and time)
2. **No partial results** — the parent can't incorporate intermediate findings; it must wait for the full result

The agent ecosystem feedback explicitly called this out: "Streaming updates from child agents would let me monitor long-running subtasks progressively, intervene early if they're going off-track, or incorporate partial results without waiting for completion."

---

## Current → Target State

**Current:** `spawn-agent` returns a `task_id`. Parent calls `await-agents` which blocks until all children reach a terminal state (`Complete`, `Failed`, `Cancelled`). No intermediate visibility.

**Target:** Child tasks emit `SubAgentProgress` events through the existing `EventBus`. A new `poll-agent` tool lets the parent non-blockingly check child status and collect streamed progress updates. The parent can also subscribe to child progress events for push-based updates.

---

## Detailed Subtasks

### 1. Add `SubAgentProgress` event type

**File:** `crates/agentos-types/src/event.rs`

Add new event variants to `EventType`:

```rust
// ── MultiAgentEvents ──
SubAgentProgress,
SubAgentCompleted,
SubAgentFailed,
```

Add them to the `category()` match returning `EventCategory::AgentCommunication`.

### 2. Add progress reporting to task executor

**File:** `crates/agentos-kernel/src/task_executor.rs`

After each LLM inference iteration in the task execution loop, emit a `SubAgentProgress` event if the task has `parent_task_id.is_some()`:

```rust
if let Some(parent_id) = task.parent_task_id {
    let progress = serde_json::json!({
        "child_task_id": task.id.to_string(),
        "parent_task_id": parent_id.to_string(),
        "iteration": iteration_count,
        "last_tool_call": last_tool_name,
        "status": "running",
        "summary": truncate(&last_assistant_message, 200),
    });
    self.emit_event(EventType::SubAgentProgress, progress, EventSeverity::Info).await;
}
```

This reuses the existing `emit_event` infrastructure — no new channels needed.

### 3. Create `poll-agent` tool

**File:** `crates/agentos-tools/src/poll_agent.rs` (new file)

A non-blocking tool that checks child task status and returns any accumulated progress events:

```rust
pub struct PollAgentTool;

#[async_trait]
impl AgentTool for PollAgentTool {
    fn name(&self) -> &str { "poll-agent" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.spawn".to_string(), PermissionOp::Execute)]
    }

    async fn execute(&self, payload: Value, _ctx: ToolExecutionContext) -> Result<Value, AgentOSError> {
        let task_ids: Vec<String> = /* extract from payload */;
        Ok(serde_json::json!({
            "_kernel_action": "poll_agents",
            "task_ids": task_ids,
        }))
    }
}
```

**Input schema:**
```json
{
  "task_ids": ["<TaskID>", ...],
  "include_progress": true
}
```

**Output schema:**
```json
{
  "results": [
    {
      "task_id": "<TaskID>",
      "state": "Running",
      "iteration": 5,
      "last_tool_call": "file-read",
      "last_update": "2026-04-07T10:30:00Z",
      "summary": "Reading configuration files to determine...",
      "progress_events": [...]
    }
  ]
}
```

### 4. Handle `poll_agents` kernel action

**File:** `crates/agentos-kernel/src/task_executor.rs`

In the `_kernel_action` dispatch block (where `spawn_agent` and `await_agents` are handled), add a `poll_agents` handler:

- Look up each `task_id` in the task store
- Validate the calling task is the parent (check `parent_task_id`)
- Collect the task's current `TaskState`, iteration count, and recent `SubAgentProgress` events from the event bus
- Return a JSON summary — do NOT block, do NOT inject into context (the tool result is sufficient)

### 5. Add `cancel-agent` tool for early intervention

**File:** `crates/agentos-tools/src/cancel_agent.rs` (new file)

Allow the parent to cancel a child that's going off-track:

```rust
pub struct CancelAgentTool;
// Returns: { "_kernel_action": "cancel_agent", "task_id": "..." }
```

The kernel handler transitions the child task to `Cancelled` state, which already cascades to grandchildren via existing `CancelTask` logic.

### 6. Register new tools and create manifests

**File:** `crates/agentos-tools/src/runner.rs` — add `PollAgentTool` and `CancelAgentTool` to `register_memory_tools()`

**Files:** `tools/core/poll-agent.toml`, `tools/core/cancel-agent.toml` — tool manifests with `trust_tier = "core"`, appropriate descriptions, and JSON schemas.

### 7. Update agent-manual coordination section

**File:** `crates/agentos-tools/src/agent_manual.rs`

Update the `Coordination` manual section to document the new tools: `poll-agent` for non-blocking progress checks, `cancel-agent` for early termination.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/event.rs` | Add `SubAgentProgress`, `SubAgentCompleted`, `SubAgentFailed` event variants |
| `crates/agentos-kernel/src/task_executor.rs` | Emit progress events for child tasks; handle `poll_agents` and `cancel_agent` kernel actions |
| `crates/agentos-tools/src/poll_agent.rs` | New `PollAgentTool` implementation |
| `crates/agentos-tools/src/cancel_agent.rs` | New `CancelAgentTool` implementation |
| `crates/agentos-tools/src/runner.rs` | Register new tools |
| `crates/agentos-tools/src/lib.rs` | Add `mod poll_agent; mod cancel_agent;` |
| `crates/agentos-tools/src/agent_manual.rs` | Update coordination section docs |
| `tools/core/poll-agent.toml` | Tool manifest |
| `tools/core/cancel-agent.toml` | Tool manifest |

---

## Dependencies

- **Requires:** Phase 1 (sub-agent spawning — `parent_task_id` field), Phase 3 (coordination tools — `_kernel_action` pattern)
- **Blocks:** Nothing — this is an additive enhancement

---

## Test Plan

1. **Progress emission test** — spawn a child task with a mock LLM that runs 3 iterations; verify 3 `SubAgentProgress` events are emitted to the event bus with correct `parent_task_id`
2. **Poll tool test** — spawn a child, call `poll-agent` while child is running; verify response includes `state: "Running"`, iteration count, and last tool call
3. **Poll authorization test** — attempt to poll a task where the caller is NOT the parent; verify `PermissionDenied` error
4. **Cancel tool test** — spawn a child, call `cancel-agent`; verify child transitions to `Cancelled` and any grandchildren are also cancelled
5. **Cancel authorization test** — attempt to cancel a task where caller is not the parent; verify rejection
6. **Completed child poll test** — poll a child that has already completed; verify response includes `state: "Complete"` and final result summary

---

## Verification

```bash
# Build
cargo build --workspace

# Run tests
cargo test -p agentos-tools -- poll_agent
cargo test -p agentos-tools -- cancel_agent
cargo test -p agentos-kernel -- sub_agent_progress

# Clippy
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all -- --check
```
