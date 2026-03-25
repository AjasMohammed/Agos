---
title: "Phase 4: Real Streaming for All Providers"
tags:
  - llm
  - v3
  - plan
date: 2026-03-24
status: planned
effort: 2d
priority: high
---

# Phase 4: Real Streaming for All Providers

> Implement real SSE/NDJSON streaming for OpenAI, Anthropic, and Gemini adapters, including tool call accumulation during streaming.

---

## Why This Phase

Only Ollama currently implements real streaming. OpenAI, Anthropic, and Gemini fall back to the `LLMCore` trait default which calls `infer()` synchronously and emits the entire response as a single `InferenceEvent::Token`. This means:

1. The web UI's SSE streaming shows nothing until the full response is ready, then dumps it all at once.
2. Tool calls during streaming are invisible -- the user sees a blank screen while a 10-second tool call runs.
3. The `Thinking...` indicator in the web UI stays on for the entire inference duration.

After this phase, all four major providers stream tokens in real-time and accumulate tool calls as they arrive.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| OpenAI streaming | Fake (trait default) | Real SSE parsing of `data: {...}` lines, tool call delta accumulation |
| Anthropic streaming | Fake (trait default) | Real SSE parsing of `event:` / `data:` lines, content block tracking |
| Gemini streaming | Fake (trait default) | Real SSE from `streamGenerateContent?alt=sse` endpoint |
| Ollama streaming | Real NDJSON (text only) | Add tool call extraction from final NDJSON line |
| `ModelCapabilities.supports_streaming` | All `false` (default) | OpenAI/Anthropic/Gemini/Ollama set `true` |
| `InferenceEvent::ToolCallStart` | Does not exist | Emitted when a tool call name is first seen in the stream |
| `InferenceEvent::ToolCallComplete` | Does not exist | Emitted when a tool call is fully assembled |

---

## What to Do

### Step 1: Implement OpenAI streaming in `openai.rs`

Override `infer_stream_with_tools`. Key implementation details:

- Set `"stream": true` in the request body.
- Read the response as a `bytes_stream()`.
- Parse each SSE line (`data: {...}`) as JSON.
- Text deltas: `choices[0].delta.content` -> emit `InferenceEvent::Token`.
- Tool call deltas: `choices[0].delta.tool_calls[i]` with `index` field.
  - First delta with `id` and `function.name` -> emit `InferenceEvent::ToolCallStart`.
  - Subsequent deltas concatenate `function.arguments` chunks.
  - On `finish_reason: "tool_calls"` or stream end, assemble final tool calls -> emit `InferenceEvent::ToolCallComplete` for each.
- Usage: `usage` object in the final chunk (with `stream_options: {"include_usage": true}` in request).
- On `data: [DONE]`, assemble `InferenceResult` and emit `InferenceEvent::Done`.

Tool call accumulation state:

```rust
struct StreamingToolCallAccumulator {
    calls: Vec<PartialToolCall>,
}

struct PartialToolCall {
    index: usize,
    id: Option<String>,
    name: String,
    arguments_buffer: String,
}
```

### Step 2: Implement Anthropic streaming in `anthropic.rs`

Override `infer_stream_with_tools`. Anthropic SSE events:

- `event: message_start` -> extract `message.usage` (input tokens).
- `event: content_block_start` -> if `content_block.type == "text"`, prepare text accumulator. If `"tool_use"`, emit `InferenceEvent::ToolCallStart` with block's `id` and `name`.
- `event: content_block_delta` -> if `delta.type == "text_delta"`, emit `Token(delta.text)`. If `"input_json_delta"`, accumulate `delta.partial_json`.
- `event: content_block_stop` -> if tool_use block, parse accumulated JSON -> emit `ToolCallComplete`.
- `event: message_delta` -> extract `stop_reason` and `usage.output_tokens`.
- `event: message_stop` -> assemble `InferenceResult` and emit `Done`.

### Step 3: Implement Gemini streaming in `gemini.rs`

Override `infer_stream_with_tools`. Use `streamGenerateContent?alt=sse` endpoint:

- Each SSE data line is a JSON object with `candidates[0].content.parts`.
- Text parts -> emit `Token`.
- `functionCall` parts -> emit `ToolCallComplete` (Gemini does not stream function call arguments).
- `usageMetadata` in final chunk -> capture token counts.
- Assemble `InferenceResult` on stream end.

### Step 4: Update Ollama streaming to handle tool calls

In `infer_stream` and add `infer_stream_with_tools`, check the final NDJSON line for `tool_calls` and emit `ToolCallComplete` events.

### Step 5: Set `supports_streaming: true` in all real-streaming adapters

In each adapter's constructor, set `capabilities.supports_streaming = true`.

### Step 6: Add `stream_options` to OpenAI request

Include `"stream_options": {"include_usage": true}` in the streaming request body so token usage is reported in the final chunk.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/openai.rs` | Implement `infer_stream_with_tools` with real SSE parsing and tool call accumulation |
| `crates/agentos-llm/src/anthropic.rs` | Implement `infer_stream_with_tools` with content block tracking |
| `crates/agentos-llm/src/gemini.rs` | Implement `infer_stream_with_tools` with `streamGenerateContent` endpoint |
| `crates/agentos-llm/src/ollama.rs` | Update `infer_stream` to emit tool call events; add `infer_stream_with_tools` |

---

## Prerequisites

[[01-core-types-and-trait-redesign]] must be complete (new `InferenceEvent` variants exist).

---

## Test Plan

- `cargo build -p agentos-llm` must pass
- `cargo test -p agentos-llm` -- existing tests pass
- Add test for each adapter: create a local TCP server that sends pre-recorded SSE responses, verify the adapter emits the correct sequence of `InferenceEvent` variants
- Specifically test OpenAI parallel tool call streaming: two tool calls interleaved by `index`, verify both are accumulated correctly
- Test Anthropic content block sequence: text block -> tool_use block -> verify both Token and ToolCallComplete events
- Test stream error handling: server sends partial response then disconnects -> verify `InferenceEvent::Error` is emitted

---

## Verification

```bash
cargo build -p agentos-llm
cargo test -p agentos-llm -- --nocapture
cargo build --workspace
cargo test --workspace
cargo clippy -p agentos-llm -- -D warnings
cargo fmt --all -- --check
```
