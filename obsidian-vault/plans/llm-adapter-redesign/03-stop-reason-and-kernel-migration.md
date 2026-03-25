---
title: "Phase 3: Stop Reason Propagation and Kernel Migration"
tags:
  - llm
  - kernel
  - v3
  - plan
date: 2026-03-24
status: planned
effort: 1.5d
priority: critical
---

# Phase 3: Stop Reason Propagation and Kernel Migration

> Make each adapter set the correct `StopReason` from provider responses, then migrate the kernel's agentic loops to use `StopReason` and structured `InferenceToolCall` instead of regex text parsing.

---

## Why This Phase

The kernel currently determines whether an LLM response contains tool calls by calling `crate::tool_call::parse_tool_call(&result.text)` -- a regex parser that looks for ````json` blocks in the response text. This is fragile because:

1. The model might produce JSON that looks like a tool call but is not.
2. The `append_legacy_blocks()` function in `tool_helpers.rs` renders native `InferenceToolCall` structs back into text, only for the kernel to re-parse them.
3. `StopReason::ToolUse` is the authoritative signal that the model wants tool execution. The text content is supplementary reasoning, not the source of truth.

After this phase, the kernel loop checks `result.stop_reason == StopReason::ToolUse` and reads `result.tool_calls` directly, eliminating the text parsing round-trip.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| OpenAI stop_reason | Not parsed | Parsed from `choices[0].finish_reason` |
| Anthropic stop_reason | Not parsed | Parsed from response `stop_reason` field |
| Gemini stop_reason | Not parsed | Parsed from `candidates[0].finishReason` |
| Ollama stop_reason | Not parsed | Inferred: tool_calls present -> ToolUse, else EndTurn |
| Kernel `chat_infer_with_tools` loop | `parse_tool_call(&result.text)` | `result.stop_reason == StopReason::ToolUse && !result.tool_calls.is_empty()` |
| Kernel `chat_infer_streaming` loop | Same regex parsing | Same StopReason check |
| Kernel `execute_task_sync` loop | `parse_tool_calls(&inference.text)` | Check `inference.stop_reason` + read `inference.tool_calls` |
| Tool result injection in chat | `ContextRole::System` with `[TOOL_RESULT: ...]` wrapper | `ContextRole::ToolResult` with `tool_call_id` and `tool_name` in metadata |

---

## What to Do

### Step 1: Parse `StopReason` in OpenAI adapter

Open `crates/agentos-llm/src/openai.rs`. In `parse_response_json`, extract `finish_reason`:

```rust
fn parse_stop_reason(json_resp: &Value) -> StopReason {
    let reason = json_resp["choices"]
        .as_array()
        .and_then(|c| c.first())
        .and_then(|c| c["finish_reason"].as_str());

    match reason {
        Some("stop") => StopReason::EndTurn,
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::ContentFilter,
        Some(other) => StopReason::Other(other.to_string()),
        None => StopReason::EndTurn,
    }
}
```

Set `stop_reason` in `parse_response_json`'s `InferenceResult` construction. Also capture `cached_tokens`:

```rust
let cached_tokens = json_resp["usage"]["prompt_tokens_details"]["cached_tokens"]
    .as_u64()
    .unwrap_or(0);
```

### Step 2: Parse `StopReason` in Anthropic adapter

Open `crates/agentos-llm/src/anthropic.rs`. In `infer_with_tools`, after parsing the response:

```rust
let stop_reason = match json_resp["stop_reason"].as_str() {
    Some("end_turn") => StopReason::EndTurn,
    Some("tool_use") => StopReason::ToolUse,
    Some("max_tokens") => StopReason::MaxTokens,
    Some("stop_sequence") => StopReason::StopSequence,
    Some(other) => StopReason::Other(other.to_string()),
    None => StopReason::EndTurn,
};

let cached_tokens = json_resp["usage"]["cache_read_input_tokens"]
    .as_u64()
    .unwrap_or(0);
```

### Step 3: Parse `StopReason` in Gemini adapter

Open `crates/agentos-llm/src/gemini.rs`. In `infer_with_tools`:

```rust
let stop_reason = json_resp["candidates"]
    .as_array()
    .and_then(|c| c.first())
    .and_then(|c| c["finishReason"].as_str())
    .map(|reason| match reason {
        "STOP" => StopReason::EndTurn,
        "FUNCTION_CALL" => StopReason::ToolUse,
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" => StopReason::ContentFilter,
        other => StopReason::Other(other.to_string()),
    })
    .unwrap_or(StopReason::EndTurn);
```

### Step 4: Set `StopReason` in Ollama adapter

Open `crates/agentos-llm/src/ollama.rs`. In `response_to_inference_result`:

```rust
let stop_reason = if !tool_calls.is_empty() {
    StopReason::ToolUse
} else {
    StopReason::EndTurn
};
```

### Step 5: Migrate kernel `chat_infer_with_tools` loop

Open `crates/agentos-kernel/src/kernel.rs`. In the `chat_infer_with_tools` method (around line 318), replace the loop body:

**Before:**
```rust
match crate::tool_call::parse_tool_call(&result.text) {
    Some(tool_call) => { /* execute tool */ }
    None => { break result.text; }
}
```

**After:**
```rust
use agentos_llm::StopReason;

let has_native_tool_calls = result.stop_reason == StopReason::ToolUse
    && !result.tool_calls.is_empty();

// Also check legacy text parsing as fallback for providers that don't set stop_reason.
let parsed_legacy = if !has_native_tool_calls {
    crate::tool_call::parse_tool_call(&result.text)
} else {
    None
};

if has_native_tool_calls {
    // Use the first native tool call. Push assistant message to context.
    ctx.push(/* assistant entry with result.text */);

    for native_call in &result.tool_calls {
        // Execute tool using native_call.tool_name, native_call.payload
        let exec_ctx = /* ... same as current ... */;
        let tool_result = self.tool_runner.execute(
            &native_call.tool_name, native_call.payload.clone(), exec_ctx
        ).await.unwrap_or_else(|e| json!({"error": e.to_string()}));

        // Inject tool result with native metadata.
        ctx.push(agentos_types::ContextEntry {
            role: agentos_types::ContextRole::ToolResult,
            content: serde_json::to_string_pretty(&tool_result).unwrap_or_default(),
            metadata: Some(agentos_types::ContextMetadata {
                tool_call_id: native_call.id.clone(),
                tool_name: Some(native_call.tool_name.clone()),
                ..Default::default()
            }),
            /* ... other fields ... */
        });

        tool_calls.push(ChatToolCallRecord { /* ... */ });
    }
} else if let Some(tool_call) = parsed_legacy {
    // Legacy fallback: text-parsed tool call.
    /* ... existing code ... */
} else {
    // No tool calls -- final answer.
    break result.text;
}
```

This approach is backward-compatible: native tool calls are preferred, legacy parsing is the fallback.

### Step 6: Apply the same migration to `chat_infer_streaming`

The streaming chat method (around line 474) has the same loop structure. Apply the same native-first, legacy-fallback pattern.

### Step 7: Apply the same migration to `execute_task_sync`

Open `crates/agentos-kernel/src/task_executor.rs`. The task executor loop (around line 2194) already reads `inference.tool_calls` for native calls and falls back to `parse_tool_calls`. Update it to also check `inference.stop_reason`:

```rust
let has_native = inference.stop_reason == StopReason::ToolUse
    && !inference.tool_calls.is_empty();
let parsed_tool_calls = if has_native {
    inference.tool_calls.iter().map(|tc| /* convert to ParsedToolCall */).collect()
} else {
    crate::tool_call::parse_tool_calls(&inference.text)
};
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/openai.rs` | Parse `finish_reason` -> `StopReason`, capture `cached_tokens` |
| `crates/agentos-llm/src/anthropic.rs` | Parse `stop_reason` -> `StopReason`, capture `cache_read_input_tokens` |
| `crates/agentos-llm/src/gemini.rs` | Parse `finishReason` -> `StopReason` |
| `crates/agentos-llm/src/ollama.rs` | Infer `StopReason` from tool_calls presence |
| `crates/agentos-kernel/src/kernel.rs` | Migrate `chat_infer_with_tools` and `chat_infer_streaming` to use `StopReason` + native tool calls |
| `crates/agentos-kernel/src/task_executor.rs` | Migrate `execute_task_sync` to prefer native tool calls |

---

## Prerequisites

- [[01-core-types-and-trait-redesign]] must be complete (`StopReason` type exists)
- [[02-native-tool-result-formatting]] must be complete (tool result injection with metadata)

---

## Test Plan

- `cargo build --workspace` must pass
- `cargo test --workspace` must pass
- Add test in `openai.rs`: `test_parse_stop_reason_tool_calls` -- response with `finish_reason: "tool_calls"` produces `StopReason::ToolUse`
- Add test in `openai.rs`: `test_parse_stop_reason_stop` -- `finish_reason: "stop"` produces `StopReason::EndTurn`
- Add test in `anthropic.rs`: `test_parse_stop_reason_tool_use` -- `stop_reason: "tool_use"` produces `StopReason::ToolUse`
- Add test in `gemini.rs`: `test_parse_stop_reason_function_call` -- `finishReason: "FUNCTION_CALL"` produces `StopReason::ToolUse`
- Existing kernel integration tests that use `MockLLMCore` must continue to pass (they produce `StopReason::EndTurn` by default, so the legacy fallback path executes)

---

## Verification

```bash
cargo build --workspace
cargo test --workspace -- --nocapture
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
