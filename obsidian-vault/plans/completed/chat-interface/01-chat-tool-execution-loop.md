---
title: Chat Tool Execution Loop
tags:
  - kernel
  - llm
  - tools
  - v3
  - plan
date: 2026-03-18
status: planned
effort: 2d
priority: critical
---

# Phase 01 -- Chat Tool Execution Loop

> Add a `chat_infer_with_tools()` method to the kernel that detects tool call JSON in LLM responses, executes the tool via `ToolRunner`, injects the result back into the context window, and re-infers until the LLM produces a final natural-language answer.

---

## Why This Phase

This is the critical bug fix. Currently, `Kernel::chat_infer()` (line 123 of `kernel.rs`) calls `llm.infer(&ctx)` exactly once and returns `result.text`. If the LLM responds with a tool call block like:

```json
{"tool": "agent-manual", "intent_type": "query", "payload": {"section": "tools"}}
```

...that raw JSON is stored as the assistant message and shown to the user. The `parse_tool_call()` function in `tool_call.rs` exists and works correctly, but is never called from the chat path.

The task executor in `task_executor.rs` has a full tool execution loop (lines 928-1080+), but it is tightly coupled to `AgentTask`, `CapabilityToken`, `IntentValidator`, injection scanning, cost tracking, and context manager persistence. We need a lighter-weight loop for interactive chat.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `Kernel::chat_infer()` | Single `llm.infer()` call; returns `result.text` | Replaced by `chat_infer_with_tools()` with tool loop |
| Tool detection in chat | None | `parse_tool_call()` called on every LLM response |
| Tool execution in chat | None | `ToolRunner::execute()` called with chat-scoped `ToolExecutionContext` |
| Return type | `Result<String, String>` | `Result<ChatInferenceResult, String>` with answer + tool call log |
| Max iterations | N/A | 10 iterations hard cap |
| Chat PermissionSet | N/A | Read/query/observe by default; configurable |

---

## What to Do

### Step 1: Define `ChatInferenceResult` and `ChatToolCallRecord`

Open `crates/agentos-kernel/src/kernel.rs`. Add these types above the `impl Kernel` block:

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
```

### Step 2: Build a default chat `PermissionSet`

Add a helper function in `kernel.rs`:

```rust
/// Build a permissive-but-safe PermissionSet for chat tool execution.
/// Grants read/query/observe on all resources. Write/execute are denied
/// unless the caller explicitly overrides.
fn chat_default_permissions() -> PermissionSet {
    PermissionSet {
        entries: vec![
            PermissionEntry {
                resource: "*".to_string(),
                read: true,
                write: false,
                execute: false,
                expires_at: None,
            },
        ],
        deny_entries: vec![],
    }
}
```

### Step 3: Implement `chat_infer_with_tools()`

Add a new method to `impl Kernel`. This method:

1. Looks up the agent and LLM adapter (same as `chat_infer`).
2. Builds the system prompt and context window (same as `chat_infer`).
3. Enters a loop (max 10 iterations):
   a. Calls `llm.infer(&ctx)`.
   b. Calls `parse_tool_call(&result.text)`.
   c. If no tool call found, this is the final answer -- break and return.
   d. If tool call found:
      - Build a `ToolExecutionContext` with a synthetic `TaskID`, the agent's `AgentID`, a fresh `TraceID`, and the chat default permissions.
      - Call `self.tool_runner.execute(tool_call.tool_name, tool_call.payload, exec_ctx)`.
      - Record the tool call in `tool_calls: Vec<ChatToolCallRecord>`.
      - Push the LLM's tool-call response as an `Assistant` entry into the context window.
      - Push the tool result as a `System` entry (prefixed with `[TOOL_RESULT: tool-name]`).
      - Continue the loop.
4. If the loop exhausts 10 iterations, return the last LLM text as the answer with a warning appended.

```rust
const CHAT_MAX_TOOL_ITERATIONS: u32 = 10;

pub async fn chat_infer_with_tools(
    &self,
    agent_name: &str,
    history: &[(String, String)],
    new_message: &str,
) -> Result<ChatInferenceResult, String> {
    // ... (agent lookup + LLM lookup + context build -- same as chat_infer) ...

    let mut tool_calls = Vec::new();
    let mut iterations = 0u32;
    let mut final_answer = String::new();

    loop {
        iterations += 1;
        if iterations > CHAT_MAX_TOOL_ITERATIONS {
            final_answer.push_str("\n\n[Note: Maximum tool call limit reached.]");
            break;
        }

        let result = llm.infer(&ctx).await.map_err(|e| format!("Inference failed: {}", e))?;

        match crate::tool_call::parse_tool_call(&result.text) {
            Some(tool_call) => {
                // Push LLM response into context
                ctx.push(ContextEntry {
                    role: ContextRole::Assistant,
                    content: result.text.clone(),
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                    importance: 0.5,
                    pinned: false,
                    reference_count: 0,
                    partition: ContextPartition::Active,
                    category: ContextCategory::Task,
                });

                // Execute the tool
                let exec_ctx = ToolExecutionContext {
                    data_dir: self.data_dir.clone(),
                    task_id: TaskID::new(),   // synthetic -- not tracked by scheduler
                    agent_id: agent_id,
                    trace_id: TraceID::new(),
                    permissions: chat_default_permissions(),
                    vault: None,
                    hal: Some(self.hal.clone()),
                    file_lock_registry: None,
                };

                let start = std::time::Instant::now();
                let tool_result = match self.tool_runner.execute(
                    &tool_call.tool_name,
                    tool_call.payload.clone(),
                    exec_ctx,
                ).await {
                    Ok(value) => value,
                    Err(e) => serde_json::json!({"error": e.to_string()}),
                };
                let duration_ms = start.elapsed().as_millis() as u64;

                tool_calls.push(ChatToolCallRecord {
                    tool_name: tool_call.tool_name.clone(),
                    intent_type: format!("{:?}", tool_call.intent_type),
                    payload: tool_call.payload.clone(),
                    result: tool_result.clone(),
                    duration_ms,
                });

                // Truncate large tool results to 4KB
                let result_str = {
                    let full = serde_json::to_string_pretty(&tool_result).unwrap_or_default();
                    if full.len() > 4096 {
                        format!("{}...[truncated]", &full[..4096])
                    } else {
                        full
                    }
                };

                ctx.push(ContextEntry {
                    role: ContextRole::System,
                    content: format!("[TOOL_RESULT: {}]\n{}\n[/TOOL_RESULT]", tool_call.tool_name, result_str),
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                    importance: 0.7,
                    pinned: false,
                    reference_count: 0,
                    partition: ContextPartition::Active,
                    category: ContextCategory::Task,
                });
            }
            None => {
                // No tool call -- this is the final answer
                final_answer = result.text;
                break;
            }
        }
    }

    Ok(ChatInferenceResult {
        answer: final_answer,
        tool_calls,
        iterations,
    })
}
```

### Step 4: Update `chat_infer` to delegate

Keep the old `chat_infer` as a thin wrapper that calls `chat_infer_with_tools` and returns just the answer string. This preserves backward compatibility with any callers.

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

### Step 5: Update the chat handler to use `chat_infer_with_tools`

Open `crates/agentos-web/src/handlers/chat.rs`. In both `new_session()` and `send()`, replace:

```rust
let response = match state.kernel.chat_infer(&agent_name, &[], &message).await {
```

with:

```rust
let result = match state.kernel.chat_infer_with_tools(&agent_name, &[], &message).await {
```

Then save `result.answer` as the assistant message and `result.tool_calls` as separate tool messages (Phase 02 adds the schema; for now, just save the answer).

### Step 6: Add unit tests

Add tests to `crates/agentos-kernel/src/kernel.rs` (or a new `tests/chat_tool_loop.rs`):

- `test_chat_tool_call_detected_and_executed`: Use `MockLLMCore` that returns a tool call block on first inference and a plain answer on second. Verify the tool was called and the final answer is returned.
- `test_chat_no_tool_call`: Verify a plain response is returned directly.
- `test_chat_max_iterations`: Mock an LLM that always returns tool calls. Verify the loop stops at 10 and returns a warning.
- `test_chat_tool_error_injected`: Mock a tool that fails. Verify the error JSON is injected and the LLM gets another chance.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/kernel.rs` | Add `ChatInferenceResult`, `ChatToolCallRecord`, `chat_default_permissions()`, `chat_infer_with_tools()`, update `chat_infer()` |
| `crates/agentos-web/src/handlers/chat.rs` | Update `new_session()` and `send()` to call `chat_infer_with_tools()` |

---

## Dependencies

None -- this is the first phase.

---

## Test Plan

- `cargo test -p agentos-kernel -- chat` must pass all four new tests.
- Manual test: Start the web server, connect an agent, send "what tools are available?" -- the LLM should call `agent-manual` and return a formatted answer, not raw JSON.
- Verify the tool call loop stops at 10 iterations by examining logs.

---

## Verification

```bash
cargo build -p agentos-kernel -p agentos-web
cargo test -p agentos-kernel -- chat --nocapture
cargo clippy -p agentos-kernel -p agentos-web -- -D warnings
```
