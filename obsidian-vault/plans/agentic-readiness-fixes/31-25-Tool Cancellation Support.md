---
title: "Tool Cancellation via CancellationToken"
tags:
  - next-steps
  - tools
  - kernel
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 4h
priority: medium
---

# Tool Cancellation via CancellationToken

> Pass a `CancellationToken` into tool execution so long-running tools (web-fetch, shell-exec) can be interrupted when a task is cancelled or timed out.

## What to Do

Once `execute()` starts on a tool, there's no way to cancel it mid-execution. Long-running tools (web-fetch with slow servers, shell-exec with expensive commands) continue running even after the task is cancelled or timed out. The tool's Future keeps running until it completes or the tokio runtime is dropped.

### Steps

1. **Add `CancellationToken` to `ToolExecutionContext`** in `crates/agentos-tools/src/traits.rs`:
   ```rust
   pub struct ToolExecutionContext {
       // ... existing fields ...
       pub cancellation_token: CancellationToken,
   }
   ```

2. **Create the token in `task_executor.rs`:**
   - Create a child `CancellationToken` from the task's cancellation token
   - Pass it in the `ToolExecutionContext`
   - When the task is cancelled/timed out, the token is automatically cancelled

3. **Check token in long-running tools:**
   - `http_client.rs`: use `tokio::select!` between the HTTP request and `token.cancelled()`
   - `web_fetch.rs`: same pattern
   - `shell_exec.rs`: monitor the child process + check token periodically
   - `data_parser.rs`: check between chunks if processing large files

4. **Graceful cancellation response:**
   - When cancelled, return `ToolOutput` with error: `"Tool execution cancelled"`
   - Don't panic or leave resources dangling
   - Clean up temp files

5. **Update `AgentTool` trait docs** to recommend checking the cancellation token in long-running operations

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/traits.rs` | Add `cancellation_token` to `ToolExecutionContext` |
| `crates/agentos-kernel/src/task_executor.rs` | Create and pass child token |
| `crates/agentos-tools/src/http_client.rs` | Check token via `select!` |
| `crates/agentos-tools/src/web_fetch.rs` | Check token via `select!` |

## Prerequisites

- [[31-07-Tool Output Size Limits]] (both address tool execution robustness)

## Verification

```bash
cargo test -p agentos-tools
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: start a slow tool → cancel the task → tool execution stops within 1 second. Tool returns cancellation error, not a hang.
