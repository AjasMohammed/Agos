---
title: LLM Adapter Redesign Research
tags:
  - llm
  - v3
  - plan
date: 2026-03-24
status: planned
effort: N/A
priority: critical
---

# LLM Adapter Redesign Research

> Deep research synthesis on what a production-grade agentic LLM adapter layer requires, analyzed against the current AgentOS implementation.

---

## 1. Current State Analysis

### What Exists

The `agentos-llm` crate has 10 source files implementing 6 adapters (OpenAI, Anthropic, Gemini, Ollama, Custom, Mock) behind a single `LLMCore` trait. Key characteristics:

| Aspect | Current State | Assessment |
|--------|--------------|------------|
| `LLMCore` trait | 7 methods: `infer`, `infer_with_tools`, `infer_stream`, `infer_stream_with_tools`, `capabilities`, `health_check`, `provider_name`/`model_name` | Reasonable surface but missing agentic primitives |
| Tool calling | Each adapter builds provider-specific tool JSON, parses tool calls, appends legacy `\`\`\`json` blocks | Works but dual-format (native + legacy text) is fragile |
| Streaming | Only Ollama implements real streaming; others fake it via `infer()` + single Token event | Major gap -- web UI streaming is fake for OpenAI/Anthropic/Gemini |
| Error handling | All errors map to `AgentOSError::LLMError { provider, reason }` | No retry logic, no circuit breaker, no rate limit handling |
| Tool results in context | Tool results injected as `ContextRole::ToolResult` mapped to `"user"` role | Not using provider-native tool result roles (OpenAI `tool`, Anthropic `tool_result`) |
| Cost tracking | `calculate_inference_cost()` exists but is never called by adapters | Dead code -- kernel cost_tracker does its own calculation |
| Token counting | Relies on provider-reported usage; no pre-flight estimation | Cannot predict context overflow before sending |
| `InferenceResult` | Has `text`, `tokens_used`, `tool_calls`, `uncertainty` | Missing: `stop_reason`, `cached_tokens`, `thinking_content` |
| `InferenceEvent` | `Token(String)`, `Done(InferenceResult)`, `Error(String)` | Missing: `ToolCallStart`, `ToolCallDelta`, `ToolCallComplete`, `Usage` |
| `ModelCapabilities` | `context_window_tokens`, `supports_images`, `supports_tool_calling`, `supports_json_mode`, `max_output_tokens` | Missing: `supports_streaming`, `supports_parallel_tools`, `supports_structured_output`, `supports_prompt_caching`, `supports_thinking` |

### Critical Gaps for Agentic Workflows

1. **No native tool result injection.** OpenAI expects `role: "tool"` with `tool_call_id`; Anthropic expects `role: "user"` with `tool_result` content blocks. Current code sends tool results as plain `"user"` messages, breaking multi-turn tool loops on providers that validate message structure.

2. **No stop reason propagation.** The kernel cannot distinguish "model finished" from "model wants to call tools" from "model hit max_tokens." This is critical for the agentic loop -- the kernel currently relies on regex-parsing the response text for `\`\`\`json` blocks.

3. **No streaming tool call accumulation.** OpenAI streams tool calls as deltas with an `index` field. Anthropic streams `content_block_start` and `content_block_delta` events. Neither is implemented -- streaming with tools silently falls back to non-streaming.

4. **No retry/resilience.** A single 429 or 503 kills the entire task. Production agentic workloads need exponential backoff with jitter, especially for rate-limited APIs.

5. **No provider failover.** If the primary provider is down, the agent is dead. No mechanism to try a secondary provider.

6. **Legacy text format coupling.** The `append_legacy_blocks()` function renders tool calls as markdown JSON blocks that the kernel then re-parses with regex. This round-trip is lossy and fragile. Native `InferenceToolCall` structs should flow directly.

7. **ContextWindow role mapping is incorrect.** `ContextRole::ToolResult` maps to `"user"` in all adapters. This prevents multi-turn tool use with OpenAI (which requires `role: "tool"` with matching `tool_call_id`) and Anthropic (which requires `tool_result` content blocks).

---

## 2. Provider-Specific Agentic Requirements

### OpenAI (Chat Completions API)

| Feature | Requirement | Current Support |
|---------|-------------|-----------------|
| `role: "tool"` messages | Each tool result must be a separate message with `role: "tool"` and `tool_call_id` matching the original call | Not implemented -- sends as `"user"` |
| Parallel tool calls | Model may emit multiple tool calls; all must be responded to before next turn | Parsed but response handling is single-call |
| `tool_choice` | `"auto"`, `"none"`, `"required"`, or specific function | Hardcoded to `"auto"` |
| `parallel_tool_calls` | Boolean to disable parallel calls | Not exposed |
| `response_format` | `json_object`, `json_schema` for structured output | Not implemented |
| Streaming deltas | Tool calls come as `delta.tool_calls[i].function.arguments` chunks with `index` field | Not implemented |
| `finish_reason` | `"stop"`, `"tool_calls"`, `"length"`, `"content_filter"` | Not propagated |
| Cached tokens | `usage.prompt_tokens_details.cached_tokens` | Not captured |
| `seed` | Deterministic output for reproducible agent behavior | Not exposed |
| Rate limit headers | `x-ratelimit-remaining-tokens`, `retry-after` | Not read |

### Anthropic (Messages API)

| Feature | Requirement | Current Support |
|---------|-------------|-----------------|
| `tool_result` content blocks | Tool results must be `role: "user"` with `[{"type": "tool_result", "tool_use_id": "...", "content": "..."}]` | Not implemented -- sends plain text |
| `stop_reason` | `"end_turn"`, `"tool_use"`, `"max_tokens"`, `"stop_sequence"` | Not propagated |
| `tool_choice` | `{"type": "auto"}`, `{"type": "any"}`, `{"type": "tool", "name": "..."}` | Hardcoded to `{"type": "auto"}` |
| Streaming events | `content_block_start`, `content_block_delta`, `content_block_stop`, `message_delta` | Not implemented |
| Extended thinking | `thinking` content blocks with chain-of-thought | Not captured |
| Prompt caching | `cache_control` on system prompt, `usage.cache_creation_input_tokens` | Not implemented |
| Fine-grained tool streaming | `fine-grained-tool-streaming-2025-05-14` header | Not implemented |
| Context management beta | Automatic old tool result clearing near limits | Not implemented |
| Rate limit headers | `anthropic-ratelimit-tokens-remaining`, `retry-after` | Not read |

### Gemini (GenerateContent API)

| Feature | Requirement | Current Support |
|---------|-------------|-----------------|
| `functionResponse` parts | Tool results sent as `{"functionResponse": {"name": "...", "response": {...}}}` in a `"user"` turn | Not implemented -- sends plain text |
| `functionCallingConfig` | `mode`: `"AUTO"`, `"ANY"`, `"NONE"` | Not exposed |
| `finishReason` | `"STOP"`, `"FUNCTION_CALL"`, `"MAX_TOKENS"`, `"SAFETY"` | Not propagated |
| Streaming | `streamGenerateContent` endpoint with SSE | Not implemented |
| System instructions | Top-level `systemInstruction` field | Implemented |
| Grounding | Google Search grounding for fact-checking | Not relevant yet |

### Ollama (Chat API)

| Feature | Requirement | Current Support |
|---------|-------------|-----------------|
| Tool results | Send as `role: "tool"` message with `content` field | Not implemented -- sends as `"user"` |
| Native tool calling | `tools` array in request, `tool_calls` in response | Implemented |
| Streaming | NDJSON streaming with `stream: true` | Implemented (text only) |
| `keep_alive` | Control model unloading timeout | Not exposed |
| `num_predict` | Max tokens for output | Not exposed |
| Tool calls in streaming | Tool calls not streamed -- only in final non-streaming response | Correctly handled |

---

## 3. Agentic Loop Architecture Research

### ReAct Loop (Reason + Act)

The standard agentic pattern:
1. LLM receives context + tools
2. LLM outputs reasoning + tool call(s)
3. Kernel executes tool(s), gets result(s)
4. Tool result(s) injected into context using **provider-native format**
5. LLM receives updated context, continues reasoning
6. Loop until: `stop_reason == end_turn`, max iterations, or budget exceeded

**Key insight:** The adapter layer must handle step 4 natively. Each provider has a different format for tool results, and the adapter must format them correctly or the provider will reject the request.

### Parallel Tool Execution

OpenAI and some Gemini models can request multiple tool calls in a single turn. The adapter must:
- Parse all tool calls from the response
- Return them all to the kernel
- Accept all results back in the correct format
- Handle partial failures (some tools succeed, others fail)

### Stop Reason Semantics

| Provider | "Keep going" | "Done" | "Truncated" | "Blocked" |
|----------|-------------|--------|-------------|-----------|
| OpenAI | `tool_calls` | `stop` | `length` | `content_filter` |
| Anthropic | `tool_use` | `end_turn` | `max_tokens` | N/A |
| Gemini | `FUNCTION_CALL` | `STOP` | `MAX_TOKENS` | `SAFETY` |
| Ollama | tool_calls present | no tool_calls | N/A | N/A |

The adapter should normalize these into a unified `StopReason` enum that the kernel uses for loop control instead of text parsing.

---

## 4. Streaming Architecture for Agents

### Current Problem

Only Ollama implements real streaming. OpenAI, Anthropic, and Gemini fall back to the trait default which calls `infer()` and emits the entire response as a single Token event. This defeats the purpose of the web UI's SSE streaming.

### Required Streaming Events

For agentic workflows, the stream must emit:
1. **Token chunks** -- text as it arrives
2. **Tool call start** -- model is beginning a tool call (with name)
3. **Tool call argument deltas** -- argument JSON chunks as they stream in
4. **Tool call complete** -- full tool call assembled
5. **Usage update** -- token counts when available
6. **Done** -- final assembled result

### Provider Streaming Formats

**OpenAI:** SSE with `data: {"choices": [{"delta": {...}}]}`. Tool calls arrive as deltas with `index` field for parallel calls. Arguments are streamed as string chunks that must be concatenated.

**Anthropic:** SSE with event types: `content_block_start` (type: text/tool_use), `content_block_delta` (text_delta/input_json_delta), `content_block_stop`, `message_delta` (stop_reason, usage), `message_stop`.

**Gemini:** SSE with `streamGenerateContent?alt=sse` endpoint. Each chunk contains partial `candidates` with `content.parts`.

**Ollama:** NDJSON with each line being a complete response object. `done: true` in the final line.

---

## 5. Resilience Patterns

### Retry Strategy

```
Retryable: 429 (rate limit), 500, 502, 503, 529 (overloaded)
Not retryable: 400 (bad request), 401 (auth), 403 (forbidden), 404 (not found)
```

Strategy: Exponential backoff with jitter. Base 1s, max 60s, max 3 attempts for non-streaming; max 1 attempt for streaming (cannot resume mid-stream).

Rate limit headers should be read to schedule retries optimally:
- OpenAI: `x-ratelimit-reset-tokens`, `retry-after`
- Anthropic: `retry-after`, `anthropic-ratelimit-tokens-reset`

### Circuit Breaker

Track consecutive failures per provider. After N failures in M seconds, mark provider as `Unhealthy` and skip it for a cooldown period. The `HealthStatus` enum already has the right variants.

### Provider Failover

When an agent connects with a primary provider, allow configuring a fallback chain. If primary is unhealthy or returns persistent errors, try the next provider in the chain.

---

## 6. Cost and Token Management

### Pre-flight Token Estimation

Before sending a request, estimate the token count to:
- Detect context overflow before hitting the API (saves cost)
- Trim context if over budget (drop oldest non-pinned entries)
- Decide whether to include tool definitions (they consume tokens)

Estimation approaches:
- Characters / 4 (rough, already in config as `chars_per_token`)
- `tiktoken` for OpenAI (exact but slow and requires Python or Rust port)
- Provider-reported cached counts from previous turns

### Per-Inference Cost Attribution

Each `InferenceResult` already has `TokenUsage`. The adapter should attach `InferenceCost` to the result using the pricing table. The kernel `cost_tracker` then just reads it instead of recalculating.

### Prompt Caching

Anthropic: Mark system prompt and tool definitions with `cache_control: {"type": "ephemeral"}`. Reduces cost by up to 90% on repeated turns.

OpenAI: Automatic prompt caching for prompts > 1024 tokens. Report `cached_tokens` in usage.

---

## 7. Security Considerations

### Tool Result Injection

The existing `injection_scanner` handles this at the kernel level. The adapter layer should:
- Never interpret tool results as instructions
- Wrap tool results in `<user_data>` tags per the system prompt convention
- Not allow tool results to modify the system prompt

### API Key Handling

Current: Keys stored in `SecretString` from the `secrecy` crate. This is correct. The adapter should also:
- Never log API keys (already handled by `SecretString`)
- Never include keys in error messages
- Zero keys on adapter drop

### Rate Limiting

The kernel already has `per_agent_rate_limiter`. The adapter should respect rate limit headers from providers and propagate them upward so the kernel can throttle accordingly.

---

## 8. Design Principles for the Redesign

1. **Native tool protocol per provider.** Each adapter must speak its provider's native tool calling protocol, not a text-based approximation.

2. **Unified stop reason.** A `StopReason` enum replaces text parsing for loop control.

3. **Rich streaming events.** `InferenceEvent` must carry tool call progress, not just text tokens.

4. **Resilience built in.** Retry and circuit breaker at the adapter level, not the kernel.

5. **Cost-aware.** Every inference reports cost. Pre-flight estimation prevents overflow.

6. **Backward compatible.** The `LLMCore` trait changes must not break existing kernel code in a single step. Migration should be phased.

7. **Testable.** `MockLLMCore` must support all new features (tool calls, stop reasons, streaming events).

---

## Sources

- [OpenAI Function Calling Documentation](https://platform.openai.com/docs/guides/function-calling)
- [Anthropic Tool Use Implementation Guide](https://platform.claude.com/docs/en/agents-and-tools/tool-use/implement-tool-use)
- [Anthropic Advanced Tool Use](https://www.anthropic.com/engineering/advanced-tool-use)
- [Gemini Function Calling Documentation](https://ai.google.dev/gemini-api/docs/function-calling)
- [Gemini Using Tools](https://ai.google.dev/gemini-api/docs/tools)
- [OpenAI Streaming Tool Calls Discussion](https://community.openai.com/t/efficiently-collecting-tool-calls-with-parallel-tool-calls-true-during-streaming/993979)
