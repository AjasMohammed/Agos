---
title: In-Process Execution Safety Hardening
tags:
  - kernel
  - sandbox
  - v3
  - plan
date: 2026-03-21
status: planned
effort: 3h
priority: high
---

# In-Process Execution Safety Hardening

> Ensure the in-process execution path for Core tools has proper audit logging of the execution mode, consistent tracing spans, and verified cancellation propagation so there is no observability or safety gap compared to the sandbox path.

---

## Why This Phase

With Phase 02 routing Core tools in-process, a significant portion of tool executions now bypass the sandbox path. The sandbox path has detailed tracing (pid, memory, cpu limits, wall time, exit code) and its results are already audit-logged. The in-process path has basic logging from `ToolRunner::execute()` but lacks:

1. **Execution mode in audit events**: The existing `ToolCallStarted`/`ToolCallCompleted` events don't record whether the tool ran in-process or sandboxed. Operators need this for debugging and compliance.
2. **Consistent tracing span fields**: The sandbox path logs `declared_memory_mb`, `rlimit_as_mb`, `max_cpu_secs`. The in-process path just logs tool name and duration. Adding a span field like `execution_mode = "in_process"` or `execution_mode = "sandbox"` makes log correlation trivial.
3. **Cancellation token propagation**: The in-process path passes `tool_cancellation` in `ToolExecutionContext`, but we should verify that `ToolRunner::execute()` and the individual tool implementations actually check it. If not, a cancelled task's tools will run to completion anyway.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `ToolCallStarted` event payload | Has `tool_name`, `task_id`, `agent_id` | Also has `execution_mode: "in_process" \| "sandbox"` |
| `ToolCallCompleted` event payload | Has `tool_name`, `duration_ms`, `success` | Also has `execution_mode` field |
| In-process tracing span | `info!(tool = tool_name, task_id, "Executing tool")` in `ToolRunner::execute()` | Additional span field: `execution.mode = "in_process"` in the `execute_parallel_tool_calls` dispatch |
| Cancellation propagation | `tool_cancellation` passed in context but not verified | Verified that `CancellationToken` is checked; document which tools support it |

## What to Do

### 1. Add execution_mode to tool call events

Open `crates/agentos-kernel/src/task_executor.rs`.

Find the `ToolCallStarted` event emission (around line 890-907). The `serde_json::json!` payload currently includes `tool_name`, `task_id`, `agent_id`. Add `execution_mode`:

Before the `join_set.spawn(async move {` block, determine the execution mode from the `sandbox_plan`:

```rust
let execution_mode = if sandbox_plan.is_some() { "sandbox" } else { "in_process" };
```

Then include it in the `ToolCallStarted` event:

```rust
self.emit_event_with_trace(
    EventType::ToolCallStarted,
    EventSource::ToolRunner,
    EventSeverity::Info,
    serde_json::json!({
        "tool_name": tool_call.tool_name,
        "task_id": task.id.to_string(),
        "agent_id": task.agent_id.to_string(),
        "execution_mode": execution_mode,
    }),
    // ... rest unchanged
)
.await;
```

Make sure to clone or copy `execution_mode` into the spawned async block so it's available for the completion event too. Since it's a `&str`, clone it as a `String`:

```rust
let execution_mode_str = execution_mode.to_string();
```

And move `execution_mode_str` into the async block.

### 2. Add execution_mode to tool result processing

Find where tool call results are processed after `join_set.join_next().await` (around line 995-1008). After the outcomes are collected, each outcome's result is processed and audit events are emitted. Add `execution_mode` to the `ToolCallCompleted` event payload.

If the `ParallelToolOutcome` struct doesn't carry the execution mode, add a field:

```rust
struct ParallelToolOutcome {
    order: usize,
    tool_call: ToolCall,
    trace_id: TraceID,
    snapshot_ref: Option<String>,
    tool_payload_preview: String,
    duration_ms: u64,
    result: Result<serde_json::Value, AgentOSError>,
    execution_mode: String,  // NEW
}
```

Set it in the spawned closure:

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

### 3. Add tracing spans for execution mode

In the spawned async block inside `execute_parallel_tool_calls`, add a tracing span around the dispatch branch:

```rust
let _span = tracing::info_span!(
    "tool_execution",
    tool = %tool_call.tool_name,
    mode = %execution_mode_str,
    task_id = %task_id,
)
.entered();
```

This ensures that all log lines from both `SandboxExecutor::spawn()` and `ToolRunner::execute()` are correlated under a single span with the execution mode.

### 4. Verify cancellation token propagation

Check that `ToolExecutionContext.cancellation_token` is actually used:

1. Open `crates/agentos-tools/src/traits.rs` -- check if the `AgentTool::execute()` trait method receives `ToolExecutionContext` which contains the token.
2. Open `crates/agentos-tools/src/shell_exec.rs` -- this is the most important tool to check since shell commands can run indefinitely. Verify it checks `cancellation_token.is_cancelled()`.
3. Open `crates/agentos-tools/src/web_fetch.rs` and `crates/agentos-tools/src/http_client.rs` -- network tools should respect cancellation.

If cancellation is NOT checked in these tools, add a note to the plan but do NOT implement cancellation in individual tools in this phase -- that's a separate effort. The key deliverable here is documenting the current state and ensuring the `tokio::time::timeout` wrapper in `execute_parallel_tool_calls` serves as the fallback cancellation mechanism (which it already does).

### 5. Document the execution mode in structured logs

No code change needed -- the tracing span from step 3 automatically appears in structured JSON logs when `log_format = "json"`. Verify this by checking that `tracing::info_span!` fields propagate to the JSON output format.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Add `execution_mode` to event payloads, `ParallelToolOutcome` struct, and tracing span |

## Prerequisites

[[02-trust-aware-dispatch]] must be complete -- the execution mode depends on `sandbox_plan_for_tool()` returning `None` for Core tools.

## Test Plan

- **Test `tool_call_event_includes_execution_mode`**: In an integration test, submit a task with a Core tool under `TrustAware` policy. Capture the `ToolCallStarted` event (via event bus subscription). Assert that `payload["execution_mode"] == "in_process"`.

- **Test `sandbox_tool_call_event_includes_sandbox_mode`**: Same test but with a Community tool. Assert `payload["execution_mode"] == "sandbox"`.

- **Verify `ParallelToolOutcome` compiles**: The struct change is internal -- existing tests that construct outcomes must be updated with the new field.

- **Manual verification of tracing spans**: Run a task with `RUST_LOG=agentos_kernel=debug` and verify log output includes `tool_execution{tool=file-reader mode=in_process task_id=...}` span.

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- --nocapture
cargo clippy -p agentos-kernel -- -D warnings
cargo fmt --all -- --check
```
