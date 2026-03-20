---
title: Add Chat Tool Execution Loop to Kernel
tags:
  - kernel
  - llm
  - tools
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 2d
priority: critical
---

# Add Chat Tool Execution Loop to Kernel

> Add a `chat_infer_with_tools()` method to the Kernel that detects tool call JSON blocks in LLM responses, executes the requested tool via ToolRunner, injects the result back into the context window, and re-infers until the LLM produces a final natural-language answer.

---

## Why This Subtask

This is the critical bug fix. The current `Kernel::chat_infer()` method at line 123 of `crates/agentos-kernel/src/kernel.rs` calls `llm.infer(&ctx)` once and returns `result.text` directly. When the LLM returns a JSON tool call block (as it is instructed to do by the system prompt at line 158), the raw JSON is saved as the assistant response and displayed to the user.

The `parse_tool_call()` function in `crates/agentos-kernel/src/tool_call.rs` correctly parses these blocks (lines 17-67, with tests at lines 74-120). The `ToolRunner::execute()` method in `crates/agentos-tools/src/runner.rs` (lines 137-183) correctly executes tools given a name, payload, and `ToolExecutionContext`. But neither is called from the chat path.

The task executor in `task_executor.rs` has a full tool loop (line 930), but it requires an `AgentTask` with `CapabilityToken`, `IntentValidator`, injection scanning, cost tracking, and context manager persistence. Chat needs a lighter-weight loop.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `Kernel::chat_infer()` signature | `pub async fn chat_infer(&self, agent_name: &str, history: &[(String, String)], new_message: &str) -> Result<String, String>` | Thin wrapper calling `chat_infer_with_tools()`, returning `Ok(result.answer)` |
| Tool detection in chat | None | `parse_tool_call(&result.text)` called on every LLM response |
| Tool execution in chat | None | `self.tool_runner.execute(name, payload, ctx)` called per tool call |
| New method | Does not exist | `pub async fn chat_infer_with_tools(...) -> Result<ChatInferenceResult, String>` |
| New types | Do not exist | `ChatInferenceResult { answer, tool_calls, iterations }`, `ChatToolCallRecord { tool_name, intent_type, payload, result, duration_ms }` |
| Max iterations | N/A | 10 hard cap constant `CHAT_MAX_TOOL_ITERATIONS` |

---

## What to Do

1. Open `crates/agentos-kernel/src/kernel.rs`.

2. Add the following types above the `impl Kernel` block (around line 112):

```rust
/// Record of a single tool call made during chat inference.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatToolCallRecord {
    pub tool_name: String,
    pub intent_type: String,
    pub payload: serde_json::Value,
    pub result: serde_json::Value,
    pub duration_ms: u64,
}

/// Result of chat inference with tool execution.
#[derive(Debug, Clone)]
pub struct ChatInferenceResult {
    /// The final natural-language answer from the LLM.
    pub answer: String,
    /// Tool calls that were executed during inference (in order).
    pub tool_calls: Vec<ChatToolCallRecord>,
    /// Total number of LLM inference iterations.
    pub iterations: u32,
}

const CHAT_MAX_TOOL_ITERATIONS: u32 = 10;
```

3. Add a helper function for building a default chat permission set:

```rust
fn chat_default_permissions() -> agentos_types::PermissionSet {
    agentos_types::PermissionSet {
        entries: vec![agentos_types::PermissionEntry {
            resource: "*".to_string(),
            read: true,
            write: false,
            execute: false,
            expires_at: None,
        }],
        deny_entries: vec![],
    }
}
```

4. Add the `chat_infer_with_tools()` method to `impl Kernel`. This method:
   - Performs the same agent/LLM lookup as `chat_infer()` (lines 129-150).
   - Builds the same system prompt and context window (lines 152-214).
   - Enters a loop with a maximum of `CHAT_MAX_TOOL_ITERATIONS` iterations.
   - Each iteration calls `llm.infer(&ctx)`, then `crate::tool_call::parse_tool_call(&result.text)`.
   - If no tool call is found, the text is the final answer -- break.
   - If a tool call is found:
     - Push the LLM response as an `Assistant` context entry.
     - Build a `ToolExecutionContext` with: `self.data_dir.clone()` for `data_dir`, `TaskID::new()` for `task_id`, the agent's `AgentID` for `agent_id`, `TraceID::new()` for `trace_id`, `chat_default_permissions()` for `permissions`, `None` for `vault`, `Some(self.hal.clone())` for `hal`, `None` for `file_lock_registry`.
     - Call `self.tool_runner.execute(&tool_call.tool_name, tool_call.payload.clone(), exec_ctx).await`.
     - On error, wrap the error in `serde_json::json!({"error": e.to_string()})`.
     - Record the call in a `Vec<ChatToolCallRecord>`.
     - Truncate the result JSON to 4096 bytes if larger.
     - Push the result as a `System` context entry: `[TOOL_RESULT: tool-name]\n{result}\n[/TOOL_RESULT]`.
     - Continue the loop.
   - If 10 iterations are exhausted, append `"\n\n[Note: Maximum tool call limit reached.]"` to the last text and break.
   - Return `ChatInferenceResult { answer, tool_calls, iterations }`.

5. Update the existing `chat_infer()` to be a thin wrapper:

```rust
pub async fn chat_infer(
    &self,
    agent_name: &str,
    history: &[(String, String)],
    new_message: &str,
) -> Result<String, String> {
    let result = self.chat_infer_with_tools(agent_name, history, new_message).await?;
    Ok(result.answer)
}
```

6. Open `crates/agentos-web/src/handlers/chat.rs`. In `new_session()` (line 135), replace the call to `state.kernel.chat_infer()` with `state.kernel.chat_infer_with_tools()`. Store `result.answer` as the assistant message. (Tool call persistence is Phase 02; for now just save the answer.)

7. In `send()` (line 223), make the same change: call `chat_infer_with_tools()` instead of `chat_infer()`, save `result.answer`.

8. Re-export the new types from the kernel's `lib.rs` if they need to be accessed by the web crate. The web crate imports `agentos_kernel::Kernel`, and since the types are defined in `kernel.rs`, they are accessed as `agentos_kernel::kernel::ChatToolCallRecord`. Add to `crates/agentos-kernel/src/lib.rs`:

```rust
pub use kernel::{ChatInferenceResult, ChatToolCallRecord};
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/kernel.rs` | Add `ChatToolCallRecord`, `ChatInferenceResult`, `CHAT_MAX_TOOL_ITERATIONS`, `chat_default_permissions()`, `chat_infer_with_tools()`. Update `chat_infer()` to delegate. |
| `crates/agentos-kernel/src/lib.rs` | Re-export `ChatInferenceResult`, `ChatToolCallRecord` |
| `crates/agentos-web/src/handlers/chat.rs` | Update `new_session()` and `send()` to call `chat_infer_with_tools()` |

---

## Prerequisites

None -- this is the first subtask.

---

## Test Plan

- `cargo test -p agentos-kernel` must pass.
- Add test `test_chat_infer_with_tools_plain_answer`:
  - Set up a `MockLLMCore` that returns `InferenceResult { text: "Hello, world!".into(), ... }`.
  - Call `chat_infer_with_tools("agent", &[], "hi")`.
  - Assert `result.answer == "Hello, world!"`, `result.tool_calls.is_empty()`, `result.iterations == 1`.
- Add test `test_chat_infer_with_tools_tool_call`:
  - Set up a `MockLLMCore` that returns a tool call JSON block on the first call, then `"Here is the result"` on the second.
  - Verify `result.answer == "Here is the result"`, `result.tool_calls.len() == 1`, `result.iterations == 2`.
- Add test `test_chat_infer_with_tools_max_iterations`:
  - Set up a `MockLLMCore` that always returns a tool call.
  - Verify `result.iterations == 10` and `result.answer` contains "Maximum tool call limit reached".
- Add test `test_chat_infer_with_tools_tool_error`:
  - Set up a `MockLLMCore` that calls a nonexistent tool on first call, then returns a plain answer.
  - Verify `result.tool_calls[0].result` contains `"error"` and `result.answer` is the plain text.

---

## Verification

```bash
cargo build -p agentos-kernel -p agentos-web
cargo test -p agentos-kernel -- chat --nocapture
cargo clippy -p agentos-kernel -p agentos-web -- -D warnings
```
