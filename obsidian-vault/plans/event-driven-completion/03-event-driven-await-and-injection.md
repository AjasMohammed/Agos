---
title: "Phase 3: Event-Driven Await and Injection"
tags:
  - kernel
  - multi-agent
  - plan
date: 2026-04-03
status: planned
effort: 1d
priority: high
---

# Phase 3: Event-Driven Await and Injection

> Rewrite `cmd_await_sub_agents` to subscribe to completion events, call `inject_sub_agent_result()` for each completed child, and return structured `SubAgentResults` with actual output text instead of state strings.

---

## Why This Phase

This phase fixes Issue #13 from [[Issues and Fixes]]: `inject_sub_agent_result()` exists in `crates/agentos-kernel/src/context.rs` (line 126) but is never called. The `cmd_await_sub_agents` handler (line 373 of `sub_agent.rs`) polls task state and returns strings like `"state=complete result=<prompt_preview>"` -- the parent LLM never sees the actual sub-agent output.

With the completion broadcast channel from Phase 2, `cmd_await_sub_agents` can now:
1. Check which children are already terminal (instant return for completed tasks).
2. Subscribe to `completion_tx` for still-running children.
3. Wait with a timeout for remaining children to complete.
4. Call `inject_sub_agent_result()` for each completed child so the output appears in the parent's context window.
5. Return structured results with actual output text.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `cmd_await_sub_agents` behavior | Polls `scheduler.get_task()` once per child; returns immediately | Subscribes to completion events; waits up to timeout for running children |
| Result format | `"state=running depth=1 agent=<id>"` strings | Structured output: `"[sub-agent 'researcher' (id) succeeded]\n<actual output>"` |
| Context injection | `inject_sub_agent_result()` never called | Called for every completed child; parent LLM sees output as `ToolResult` entry |
| `SubAgentResult` usage | Struct exists but never constructed | Constructed from `TaskCompletionEvent` data + agent name lookup |
| Timeout handling | No timeout; returns snapshot instantly | Waits up to parent task's remaining timeout; returns partial results on timeout |

## What to Do

### 1. Rewrite `cmd_await_sub_agents`

Open `crates/agentos-kernel/src/commands/sub_agent.rs`. Replace the existing `cmd_await_sub_agents` method (lines 373-429) with:

```rust
pub(crate) async fn cmd_await_sub_agents(
    &self,
    parent_task_id: TaskID,
    child_task_ids: &[TaskID],
) -> KernelResponse {
    tracing::debug!(
        parent_task_id = %parent_task_id,
        child_count = child_task_ids.len(),
        "AwaitSubAgents: waiting for child task completion"
    );

    let mut results: Vec<(TaskID, String)> = Vec::with_capacity(child_task_ids.len());
    let mut pending: Vec<TaskID> = Vec::new();

    // Step 1: Check which children are already terminal.
    for &child_id in child_task_ids {
        match self.scheduler.get_task(&child_id).await {
            Some(task) if task.state.is_terminal() => {
                let output = self.scheduler.get_task_result(&child_id).await
                    .map(|r| r.answer.clone())
                    .unwrap_or_else(|| format!("(no result stored for {})", child_id));
                let success = task.state == TaskState::Complete;
                
                // Inject into parent context.
                let agent_name = self.resolve_agent_name(task.agent_id).await;
                let sub_result = SubAgentResult {
                    child_task_id: child_id,
                    agent_name: agent_name.clone(),
                    output: output.chars().take(8192).collect(),
                    success,
                };
                if let Err(e) = self.context_manager
                    .inject_sub_agent_result(parent_task_id, &sub_result).await
                {
                    tracing::warn!(
                        parent_task_id = %parent_task_id,
                        child_task_id = %child_id,
                        error = %e,
                        "AwaitSubAgents: failed to inject result into parent context"
                    );
                }
                
                let state_label = if success { "complete" } else { "failed" };
                results.push((child_id, format!(
                    "state={} agent={} output={}",
                    state_label, agent_name,
                    sub_result.output.chars().take(500).collect::<String>()
                )));
            }
            Some(_) => {
                pending.push(child_id);
            }
            None => {
                results.push((child_id, "not_found".to_string()));
            }
        }
    }

    // Step 2: If all children are terminal, return immediately.
    if pending.is_empty() {
        return KernelResponse::SubAgentResults { results };
    }

    // Step 3: Subscribe to completion events and wait for pending children.
    let mut rx = self.completion_tx.subscribe();
    let timeout = Duration::from_secs(30); // bounded wait per poll
    let deadline = tokio::time::Instant::now() + timeout;

    while !pending.is_empty() && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => {
                if let Some(pos) = pending.iter().position(|&id| id == event.task_id) {
                    pending.remove(pos);
                    let success = event.final_state == TaskState::Complete;
                    let agent_name = self.resolve_agent_name(event.agent_id).await;
                    let sub_result = SubAgentResult {
                        child_task_id: event.task_id,
                        agent_name: agent_name.clone(),
                        output: event.output.chars().take(8192).collect(),
                        success,
                    };
                    if let Err(e) = self.context_manager
                        .inject_sub_agent_result(parent_task_id, &sub_result).await
                    {
                        tracing::warn!(
                            parent_task_id = %parent_task_id,
                            child_task_id = %event.task_id,
                            error = %e,
                            "AwaitSubAgents: failed to inject result"
                        );
                    }
                    let state_label = if success { "complete" } else { "failed" };
                    results.push((event.task_id, format!(
                        "state={} agent={} output={}",
                        state_label, agent_name,
                        sub_result.output.chars().take(500).collect::<String>()
                    )));
                }
                // Ignore events for tasks we're not waiting on.
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!(
                    parent_task_id = %parent_task_id,
                    lagged = n,
                    "AwaitSubAgents: completion event receiver lagged"
                );
            }
            Ok(Err(_)) | Err(_) => break, // channel closed or timeout
        }
    }

    // Step 4: Any still-pending children get "running" status.
    for child_id in pending {
        results.push((child_id, "state=running".to_string()));
    }

    KernelResponse::SubAgentResults { results }
}
```

### 2. Add `resolve_agent_name` helper

In `crates/agentos-kernel/src/commands/sub_agent.rs`, add a helper:

```rust
/// Resolve an AgentID to its registered name, or return the UUID string.
async fn resolve_agent_name(&self, agent_id: AgentID) -> String {
    let registry = self.agent_registry.read().await;
    registry.get_by_id(&agent_id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| agent_id.to_string())
}
```

### 3. Add `is_terminal()` to `TaskState`

Open `crates/agentos-types/src/task.rs`. Add:

```rust
impl TaskState {
    /// Returns true if this state is terminal (no further transitions expected).
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Complete | TaskState::Failed | TaskState::Cancelled)
    }
}
```

### 4. Import `SubAgentResult` in sub_agent.rs

Open `crates/agentos-kernel/src/commands/sub_agent.rs`. Ensure the imports include:

```rust
use agentos_types::{SubAgentResult, TaskCompletionEvent};
use std::time::Duration;
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/commands/sub_agent.rs` | Rewrite `cmd_await_sub_agents`; add `resolve_agent_name` helper |
| `crates/agentos-types/src/task.rs` | Add `TaskState::is_terminal()` method |

## Prerequisites

[[02-completion-event-emission]] must be complete first -- this phase subscribes to the `completion_tx` broadcast channel added in Phase 2.

## Test Plan

- **Unit test `test_await_already_completed_children`:** Spawn two child tasks, mark both as `Complete` with stored results, call `cmd_await_sub_agents`. Assert: (a) both results returned, (b) `inject_sub_agent_result` was called (verify by checking parent context entry count increased by 2).
- **Unit test `test_await_mixed_states`:** Spawn three children: one `Complete`, one `Running`, one `Failed`. Call `cmd_await_sub_agents`. Assert: completed and failed children have output text; running child has `"state=running"`.
- **Unit test `test_await_injects_into_parent_context`:** Create a parent context window, spawn one child that completes. Call `cmd_await_sub_agents`. Read parent context via `context_manager.get_context()`. Assert the last entry has `role == ToolResult` and content contains `"[sub-agent 'name'"`.
- **Unit test `test_is_terminal`:** Assert `TaskState::Complete.is_terminal() == true`, `TaskState::Running.is_terminal() == false`, etc.
- **Integration test `test_full_spawn_await_cycle`:** Spawn a sub-agent via `cmd_spawn_sub_agent`, wait for it to complete via completion events, call `cmd_await_sub_agents`. Assert the parent's context window contains the child's output.

## Verification

```bash
cargo build -p agentos-kernel -p agentos-types
cargo test -p agentos-kernel -- --nocapture
cargo test -p agentos-types -- --nocapture
cargo clippy -p agentos-kernel -p agentos-types -- -D warnings
cargo fmt --all -- --check
```
