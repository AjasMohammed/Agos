---
title: In-Process Safety Hardening
tags:
  - kernel
  - sandbox
  - v3
  - next-steps
date: 2026-03-21
status: planned
effort: 3h
priority: high
---

# In-Process Safety Hardening

> Add execution mode tracking to tool call events and tracing spans so operators can distinguish sandbox vs in-process execution in logs and audit trails.

---

## Why This Subtask

With Core tools now running in-process (Phase 02), operators need to know which execution path each tool took for debugging and compliance. The existing `ToolCallStarted`/`ToolCallCompleted` events don't record the execution mode. This subtask adds that field and adds tracing spans for log correlation.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `ToolCallStarted` event JSON | `{ tool_name, task_id, agent_id }` | Adds `execution_mode: "in_process" \| "sandbox"` |
| `ParallelToolOutcome` struct | No execution mode field | Has `execution_mode: String` field |
| Tracing in dispatch block | No span wrapping the dispatch | `info_span!("tool_execution", tool, mode, task_id)` wraps the branch |

## What to Do

1. Open `crates/agentos-kernel/src/task_executor.rs`

2. Find `execute_parallel_tool_calls()`. In the `for call in prepared` loop (around line 870), after `let sandbox_plan = call.sandbox_plan;`, add:
```rust
let execution_mode = if sandbox_plan.is_some() { "sandbox" } else { "in_process" };
let execution_mode_str = execution_mode.to_string();
```

3. Find the `ToolCallStarted` event emission (around line 890). Add `execution_mode` to the JSON:
```rust
serde_json::json!({
    "tool_name": tool_call.tool_name,
    "task_id": task.id.to_string(),
    "agent_id": task.agent_id.to_string(),
    "execution_mode": execution_mode,
}),
```

4. Find the `ParallelToolOutcome` struct definition. Add a field:
```rust
execution_mode: String,
```

5. In the `join_set.spawn(async move {` block, move `execution_mode_str` into the closure. Add a tracing span at the top of the async block:
```rust
join_set.spawn(async move {
    let _span = tracing::info_span!(
        "tool_execution",
        tool = %tool_call.tool_name,
        mode = %execution_mode_str,
        task_id = %task_id,
    )
    .entered();

    // ... existing code ...
```

6. At the end of the async block where `ParallelToolOutcome` is constructed, add the field:
```rust
ParallelToolOutcome {
    order,
    tool_call,
    trace_id,
    snapshot_ref,
    tool_payload_preview,
    duration_ms: tool_start.elapsed().as_millis() as u64,
    result,
    execution_mode: execution_mode_str,
}
```

7. Find where outcomes are processed after `join_set.join_next().await` (the loop that processes results and emits completion events). When the `ToolCallCompleted` event is emitted, include `execution_mode`:
```rust
"execution_mode": outcome.execution_mode,
```

Search for `ToolCallCompleted` or `ToolCallSuccess` or similar event type in the same function to find the exact location.

8. Verify cancellation token propagation (review only, no code changes):
   - Check `crates/agentos-tools/src/shell_exec.rs` -- does `execute()` check `context.cancellation_token`?
   - Check `crates/agentos-tools/src/web_fetch.rs` -- same check
   - The `tokio::time::timeout` wrapper in `execute_parallel_tool_calls` already serves as a hard timeout for in-process tools, so missing per-tool cancellation is not a safety issue, just an efficiency one.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Add `execution_mode` to events, `ParallelToolOutcome`, and tracing span |

## Prerequisites

[[34-02-Trust-Aware Dispatch]] must be complete so `sandbox_plan` correctly reflects the trust-tier-aware decision.

## Test Plan

- `cargo build -p agentos-kernel` compiles (struct change is backward-compatible within the file)
- All existing `cargo test -p agentos-kernel` tests pass
- Manual: run kernel with `RUST_LOG=agentos_kernel=debug`, submit task with Core tool, verify:
  - Log shows `tool_execution{tool=file-reader mode=in_process task_id=...}`
  - Event payload includes `"execution_mode": "in_process"`

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- --nocapture
cargo clippy -p agentos-kernel -- -D warnings
cargo fmt --all -- --check
```
