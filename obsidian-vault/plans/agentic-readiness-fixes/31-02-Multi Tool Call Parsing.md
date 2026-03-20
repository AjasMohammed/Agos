---
title: "Multi-Tool Call Parsing and Parallel Execution"
tags:
  - next-steps
  - kernel
  - tool-call
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 6h
priority: critical
---

# Multi-Tool Call Parsing and Parallel Execution

> Parse ALL valid tool call JSON blocks from a single LLM response and execute them in parallel, instead of extracting only the first match.

## What to Do

Currently `tool_call.rs` uses a regex to extract the **first** JSON block matching a tool call pattern. Modern LLMs (Claude, GPT-4) can emit multiple tool calls in a single response. Without parallel tool call support, every action costs a full inference round-trip, increasing latency and cost by 2-5x.

### Steps

1. **Modify `parse_tool_call()` in `crates/agentos-kernel/src/tool_call.rs`:**
   - Change return type from `Option<ToolCallRequest>` to `Vec<ToolCallRequest>` (or create new `parse_tool_calls()`)
   - Use `regex.find_iter()` instead of `regex.find()` to collect ALL matches
   - Parse each match independently, skip invalid ones with a warning log
   - Return all successfully parsed tool calls

2. **Create `ToolCallRequest` struct** if not already present:
   ```rust
   pub struct ToolCallRequest {
       pub tool_name: String,
       pub intent_type: IntentType,
       pub payload: serde_json::Value,
   }
   ```

3. **Update the agentic loop in `task_executor.rs`:**
   - After LLM inference, call the new multi-parse function
   - If multiple tool calls returned:
     - Validate ALL calls before executing any (capability + schema check)
     - If any validation fails, skip that call and log a warning
     - Execute all valid calls in parallel via `tokio::JoinSet`
     - Collect all results
     - Inject all results into the context window
   - If single tool call, execute as before (backward compatible)

4. **Add size limit on parsed payload** — reject individual payloads > 64 KiB

5. **Add config toggle** in `config/default.toml`:
   ```toml
   [kernel.tool_calls]
   allow_parallel = true
   max_parallel = 5
   ```

6. **Update tests** — add test for multi-call parsing and parallel execution

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/tool_call.rs` | New `parse_tool_calls()` returning `Vec<ToolCallRequest>` |
| `crates/agentos-kernel/src/task_executor.rs` | Parallel execution via `JoinSet` |
| `config/default.toml` | Add `[kernel.tool_calls]` section |
| `crates/agentos-kernel/src/kernel.rs` | Parse new config section |

## Prerequisites

None — can be done in parallel with [[31-01-Configurable Max Iterations Per Task]].

## Verification

```bash
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: mock LLM response containing 3 valid JSON tool calls → all 3 are parsed and executed. Single-call responses still work identically.
