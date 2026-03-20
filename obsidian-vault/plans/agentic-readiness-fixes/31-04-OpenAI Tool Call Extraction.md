---
title: "OpenAI Tool Call Extraction"
tags:
  - next-steps
  - llm
  - openai
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 4h
priority: critical
---

# OpenAI Tool Call Extraction

> Implement tool call extraction from OpenAI API responses so OpenAI-backed agents can actually use tools.

## What to Do

The OpenAI adapter in `agentos-llm/src/openai.rs` sets `supports_tool_calling: true` in capabilities but **never parses the `tool_calls` array** from the API response. It only reads `choices[0].message.content` as plain text. This means OpenAI-backed agents are completely non-functional for tool-use workflows.

### Steps

1. **Read the current OpenAI adapter** at `crates/agentos-llm/src/openai.rs` to understand the response parsing structure.

2. **Add tool call extraction** from the OpenAI response JSON:
   - Parse `choices[0].message.tool_calls` array
   - Each tool call has: `id`, `type: "function"`, `function: { name, arguments }`
   - Convert each to an `IntentMessage` or the intermediate `ToolCallRequest` format
   - If both `content` and `tool_calls` are present, include the content as reasoning text

3. **Map OpenAI tool call format to AgentOS format:**
   ```rust
   // OpenAI format:
   // { "id": "call_abc", "type": "function", "function": { "name": "file-reader", "arguments": "{\"path\": \"test.txt\"}" }}
   //
   // AgentOS format:
   // IntentMessage with target: Tool("file-reader"), payload: {"path": "test.txt"}
   ```

4. **Send tool definitions in the request** — convert `ToolManifest` list to OpenAI's `tools` parameter format:
   ```json
   { "type": "function", "function": { "name": "file-reader", "description": "...", "parameters": { ... } } }
   ```

5. **Handle `tool_choice`** — use `"auto"` by default (let the model decide)

6. **Add tests** with mock OpenAI responses containing tool calls

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/openai.rs` | Parse `tool_calls` from response, send tools in request |
| `crates/agentos-llm/src/types.rs` | Ensure `InferenceResult` can carry multiple tool calls |

## Prerequisites

- [[31-02-Multi Tool Call Parsing]] (recommended but not required — can extract first tool call as fallback)

## Verification

```bash
cargo test -p agentos-llm
cargo clippy --workspace -- -D warnings
```

Test: mock OpenAI response with `tool_calls` array → tool calls extracted and converted to IntentMessages. Mock response with only `content` → still works as text response.
