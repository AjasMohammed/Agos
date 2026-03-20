---
title: "Tool Output Size Limits and Execution Timeout"
tags:
  - next-steps
  - kernel
  - reliability
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 3h
priority: high
---

# Tool Output Size Limits and Execution Timeout

> Add output size caps and per-tool execution timeouts to prevent OOM and task-loop hangs from misbehaving tools.

## What to Do

The agentic loop injects tool output directly into the context window with no size check. A tool returning megabytes of data can cause OOM or blow the token budget. Additionally, there is no explicit timeout on tool execution — the loop relies on sandbox timeouts which aren't always present.

### Steps

1. **Add output size limit in `task_executor.rs`:**
   - After receiving `ToolOutput` from `ToolRunner::execute()`, check byte length
   - Default max: 256 KiB (configurable via `config/default.toml`)
   - If exceeded: truncate with a `[TRUNCATED: output was {n} bytes, limit {limit}]` suffix
   - Log a warning via `tracing::warn`

2. **Add per-tool execution timeout in `task_executor.rs`:**
   - Wrap `tool_runner.execute()` in `tokio::time::timeout()`
   - Default: 60 seconds (configurable)
   - Read from manifest `sandbox.timeout_seconds` if present, else use default
   - On timeout: return `ToolOutput` with error message, don't crash the loop

3. **Add config:**
   ```toml
   [kernel.tool_execution]
   max_output_bytes = 262144    # 256 KiB
   default_timeout_seconds = 60
   ```

4. **Add tests** — mock tool returning 1 MiB output → verify truncation. Mock slow tool → verify timeout.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Add output truncation + timeout wrapper |
| `config/default.toml` | Add `[kernel.tool_execution]` section |
| `crates/agentos-kernel/src/kernel.rs` | Parse new config |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```
