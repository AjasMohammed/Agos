---
title: Output Sanitization Research
tags:
  - kernel
  - chat
  - llm
  - security
  - research
date: 2026-04-11
status: complete
effort: 2h
priority: high
---

# Output Sanitization Research

> Findings from reading OpenClaw, NotebookLM-cited frameworks, and AgentOS code on how production agent systems prevent internal tool-call scaffolding from leaking into user-visible chat text.

---

## Source 1 — OpenClaw (https://github.com/openclaw/openclaw)

OpenClaw treats output sanitization as a first-class problem with a six-layer defense pipeline.

### Layer A — Channel separation at the transport

Dedicated provider transports respect the structured tool-call channel:
- `src/agents/anthropic-transport-stream.ts` (866 lines) — uses `content_block_start.type` to route `text` vs `tool_use` blocks
- `src/agents/openai-transport-stream.ts` (1,386 lines) — uses `delta.tool_calls` vs `delta.content`
- Plus `google-transport-stream.ts`, `openai-ws-stream.ts`

AgentOS already does this correctly in [[../../crates/agentos-llm/src/anthropic.rs|anthropic.rs]] lines 614-720.

### Layer B — `<final>` tag enforcement (the clever part)

`src/agents/pi-embedded-subscribe.ts:484-527` — when `enforceFinalTag` is on, **only text appearing inside a `<final>...</final>` block is shown to the user**. Everything else (model thinking out loud, accidentally pasted JSON, status banter) is silently dropped.

```ts
// "Strict Mode: If enforcing final tags, we MUST NOT return content unless
// we have seen a <final> tag. Otherwise, we leak 'thinking out loud' text"
if (!everInFinal) {
  return "";
}
```

State (`state.final`, `state.thinking`, `state.inlineCode`) is carried across chunk boundaries so a tag opened in chunk 3 and closed in chunk 7 still works.

### Layer C — Output sanitizer pipeline (`src/shared/text/assistant-visible-text.ts`)

Stateful sanitizer pipeline with three profiles:

| Profile | Purpose | Strictness |
|---|---|---|
| `delivery` | Text shown to end users | Strictest — strips everything |
| `history` | Stored conversation history | Loose — preserves layout for replay |
| `internal-scaffolding` | Internal debugging views | Preserves most for transparency |

Each profile runs through these stages:

| Function | What it strips | Lines |
|---|---|---|
| `stripReasoningTagsFromText` | `<think>...</think>` blocks across chunk boundaries | reasoning-tags.ts |
| `stripToolCallXmlTags` | `<tool_call>`, `<tool_result>`, `<function_call>`, `<function_calls>`, `<tool_calls>` when followed by JSON or `<invoke>`/`<parameters>` payload | assistant-visible-text.ts:168-260 |
| `stripMinimaxToolCallXml` | Model-specific: `<minimax:tool_call>` and `<invoke>` blocks Minimax leaks | 267-279 |
| `stripDowngradedToolCallText` | Markdown leaks: `[Tool Call: name (ID: ...)]`, `[Tool Result for ID ...]`, `[Historical context: ...]` | 285-440 |
| `stripModelSpecialTokens` | `<\|im_start\|>`, `<\|endoftext\|>`, tokenizer artifacts | model-special-tokens.ts |
| `stripRelevantMemoriesTags` | `<relevant-memories>` blocks (their RAG injection format) | 442-477 |
| `stripInternalRuntimeContext` | Their own delimited blocks `<<<BEGIN_OPENCLAW_INTERNAL_CONTEXT>>>...<<<END...>>>` with **nesting depth tracking** | internal-runtime-context.ts:40-74 |

### Layer D — Code-region awareness

Every stripper uses `findCodeRegions` + `isInsideCode` so it does NOT strip tool-call syntax appearing inside fenced markdown code blocks. If the user asks "show me an example `<tool_call>` tag," the model can answer. **This is the difference between a working filter and a broken one.**

### Layer E — Error-payload rewriting (`pi-embedded-helpers/errors.ts:1261`)

```ts
export function sanitizeUserFacingText(text: unknown, opts?: { errorContext?: boolean }): string {
  const stripped = stripInternalRuntimeContext(stripFinalTagsFromText(raw));
  // ...
  if (!errorContext && shouldRewriteRawPayloadWithoutErrorContext(trimmed)) {
    return formatRawAssistantErrorForUi(trimmed);
  }
}
```

Catches Cloudflare HTML pages, raw `{"error": {...}}` payloads, billing errors, rate-limit errors, "context overflow" errors, tool-input-missing errors. Each gets a curated user-friendly rewrite instead of leaking the raw upstream payload.

### Layer F — Defensive escaping of internal delimiters

`internal-runtime-context.ts:19-23` — when injecting any user/model-supplied content into a runtime context block, OpenClaw escapes its own delimiter strings so the model can't smuggle a fake `<<<END_OPENCLAW_INTERNAL_CONTEXT>>>` to break out. Same threat model as SQL injection.

---

## Source 2 — NotebookLM (cited frameworks)

From the AI Agent Frameworks notebook query on 2026-04-11:

- **PydanticAI** — schema-first validation. Streamed structured outputs validate against Pydantic schema in real time; malformed/unstructured output fails immediately. Architecturally impossible for internal reasoning JSON to be returned as prose if it's not in the schema.
- **LangGraph** — graph-node isolation. Tool execution is a separate node from the user-facing "writer" node. Writer only sees processed findings via state object, not raw tool output.
- **LangGraph 1.0** — pre/post model hooks. Developers add a post-model hook that scans `content` for leaked JSON patterns and either strips them or triggers a retry with stricter formatting.
- **CrewAI** — "Force Tool Output as Result." Tool result IS the final answer for that task, bypassing the need for the model to wrap the result in conversational text.
- **OpenAI Agents SDK** — Guardrails. Pre/post model hooks enforce constraints on what the model is allowed to emit.

NotebookLM had no specific information on OpenHands or OpenClaw internals (only confirmed OpenClaw exists in the Fast.io ecosystem).

---

## Source 3 — AgentOS current state

What AgentOS already has matching the OpenClaw layers:
- **Layer A** ✅ Provider channel separation in [[../../crates/agentos-llm/src/anthropic.rs|anthropic.rs]] (`content_block_start.type`), [[../../crates/agentos-llm/src/openai.rs|openai.rs]] (`delta.tool_calls`), [[../../crates/agentos-llm/src/ollama.rs|ollama.rs]] (`message.tool_calls`)
- **Untrusted-data wrapping** — `<user_data>` tags + system prompt instruction in [[../../crates/agentos-kernel/src/system_prompt.rs|system_prompt.rs]] line 124
- **Injection scanner** — `injection_scanner.rs` with NFC normalization

What AgentOS is missing:
- **Layer B** ❌ No `<final>` enforcement
- **Layer C — fenced JSON tool block extraction** ❌ The system prompt at lines 76-83 instructs models to emit ` ```json ` tool blocks, but no code parses them back when adapters fail to populate `tool_calls`. **This is the proximate cause of the leak the user reported.**
- **Layer C — XML tool tag stripper** ❌ Nothing scrubs `<tool_call>`, `<function_call>`, `<invoke>` if a model emits them as plain text
- **Layer C — downgraded text stripper** ❌ Nothing handles markdown-style `[Tool Call: ...]` leaks
- **Layer D** ❌ No code-region awareness on filters
- **Layer E** ❌ No error-payload rewriting; raw provider errors leak through
- **Layer F** ❌ No outbound delimiter escaping for context-compiler markers

---

## Conclusion

The minimum viable fix for the immediate leak is to add a fenced ` ```json ` tool-block extractor that runs both as a streaming filter (during `chat_infer_streaming`) and as a post-process on `result.text`. That's Phase 1.

The other six layers from OpenClaw map to follow-up phases.

## Related

- [[Output Sanitization Plan]]
- [[01-fenced-tool-block-extractor]]
