---
title: Phase 1 — Fenced Tool-Block Extractor
tags:
  - kernel
  - chat
  - llm
  - security
  - phase
date: 2026-04-11
status: in-progress
effort: 1d
priority: high
---

# Phase 1 — Fenced Tool-Block Extractor

> Add a kernel-side sanitizer that hides leaked ` ```json ` tool intent blocks from the chat stream and promotes them to the structured tool-call channel for execution.

---

## Why this phase

The user reported seeing this in the AgentOS chat UI:

````
I'll run a live demonstration ...

```json
{"tool": "web-search", "intent_type": "query", "payload": {...}}
{"tool": "agent-list", "intent_type": "query", "payload": {...}}
```
````

The model followed the documented protocol from [[../../../crates/agentos-kernel/src/system_prompt.rs|system_prompt.rs]] line 76-83, which tells agents to use ` ```json ` blocks for tool calls. But the chat handler at [[../../../crates/agentos-kernel/src/kernel.rs|kernel.rs]] line 1554 only inspects `result.tool_calls` (the structured native channel returned by the LLM adapter). When a model puts the call in `result.text` instead, the JSON is rendered as visible text and the tool never runs.

This phase closes both halves of that gap: it strips the leak from visible text **and** promotes the parsed intents into `result.tool_calls` so they actually execute.

## Current → target state

### Current

```text
LLM stream
   │
   ▼
ChatStreamEvent::TextChunk { text: chunk }   ← raw chunk forwarded as-is
   │
   ▼
SSE frame to browser                          ← user sees ```json blocks
```

After streaming completes:

```text
result.tool_calls.is_empty()  ← adapter returned no native calls
   │
   ▼
result.text persisted verbatim                ← chat history keeps the leak
```

### Target

```text
LLM stream
   │
   ▼
OutputSanitizerStream::push(chunk)            ← stateful filter, hides matched blocks
   │
   ▼
ChatStreamEvent::TextChunk { text: cleaned }
   │
   ▼
SSE frame to browser                          ← clean prose only
```

After streaming completes:

```text
result.tool_calls.is_empty()
   │
   ▼
extract_tool_intent_blocks(&result.text)
   │
   ├── extracted: Vec<ExtractedToolIntent>    ← promoted to result.tool_calls
   └── cleaned: String                        ← persisted to chat store
```

## Detailed subtasks

### 1. Create `crates/agentos-kernel/src/output_sanitizer.rs`

A self-contained module with no kernel-state dependencies. Pure text processing + a stateful streaming filter.

**Public API:**

```rust
//! Server-side sanitization of LLM output to prevent internal tool-call
//! scaffolding (fenced ```json intent blocks, etc.) from leaking into
//! user-visible chat text.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool-call intent extracted from a fenced ```json block in LLM output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedToolIntent {
    pub tool: String,
    pub intent_type: String,
    pub payload: Value,
}

/// Result of extracting fenced tool intent blocks from a complete text.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionResult {
    /// The text with all matched blocks removed.
    pub cleaned_text: String,
    /// Tool intents extracted from removed blocks, in source order.
    pub extracted: Vec<ExtractedToolIntent>,
}

/// Scan a complete text for fenced ```json blocks whose contents parse as
/// AgentOS tool intents (`{"tool": ..., "intent_type": ..., "payload": ...}`).
/// Matching blocks are removed from the text and returned as structured
/// intents. Non-matching JSON blocks (e.g., tutorial code samples with
/// unrelated JSON) are left in place.
pub fn extract_tool_intent_blocks(text: &str) -> ExtractionResult { ... }

/// Stateful streaming filter that hides fenced tool intent blocks from
/// streamed text chunks. Buffers across chunk boundaries so a fence opened
/// in one chunk and closed in another is handled correctly.
///
/// Hide-only — does NOT execute tool calls. Use [`extract_tool_intent_blocks`]
/// on the complete text after streaming finishes to promote leaked intents
/// into the structured tool-call channel for execution.
pub struct OutputSanitizerStream {
    pending: String,
    in_fence: bool,
    fence_body_start: usize,
    fence_lang: String,
    suppressed: usize,
}

impl OutputSanitizerStream {
    pub fn new() -> Self { ... }

    /// Push the next chunk of text. Returns the portion that should be
    /// emitted to the user (may be empty if all current content is buffered
    /// inside an open fence).
    pub fn push(&mut self, chunk: &str) -> String { ... }

    /// Flush any remaining buffered content. Call exactly once after the
    /// stream ends. If the stream ended with an unclosed fence, the buffered
    /// content is emitted as-is (we don't know whether it would have been a
    /// tool block, so emit rather than silently drop).
    pub fn flush(&mut self) -> String { ... }

    /// Number of fenced blocks suppressed during this stream.
    pub fn suppressed_count(&self) -> usize { self.suppressed }
}

impl Default for OutputSanitizerStream {
    fn default() -> Self { Self::new() }
}
```

**Algorithm — `extract_tool_intent_blocks`:**

1. Walk the text character-by-character (byte indices, ASCII boundaries safe).
2. Find each occurrence of ` ``` ` (three backticks) at the start of a line or after whitespace.
3. After the opening fence, optionally consume a language tag (alphanumeric, until newline).
4. Read the fenced body up to the next ` ``` ` on its own line or after whitespace.
5. Try to parse the body as JSON. The body may contain multiple consecutive JSON objects (the user's example showed three on separate lines). Use `serde_json::Deserializer::from_str(...).into_iter::<Value>()` to parse all of them.
6. If **every** parsed value is a tool intent (object with `tool: string`, `intent_type: string`, `payload: any`), drop the entire fenced block and append the parsed intents to the result.
7. If any parsed value is not a tool intent, leave the entire block in place (mixed content — safer not to touch).
8. If the body is not valid JSON, leave it in place.

**Algorithm — `OutputSanitizerStream::push`:**

1. Append `chunk` to `self.pending`.
2. Loop:
   - If `!self.in_fence`:
     - Find the next ` ``` ` in `pending` from the current cursor.
     - If found: emit text up to it, set `in_fence = true`, advance past the opening fence (consume optional language tag too), record `fence_body_start`.
     - If not found: emit text up to the last 2 characters of `pending` (since 2 backticks could be the start of a fence in the next chunk). Hold the trailing 0-2 chars. Break.
   - If `self.in_fence`:
     - Find the next ` ``` ` in `pending` from the current cursor.
     - If found: extract the fenced body (`pending[fence_body_start..pos]`), try parsing as tool intents. If all match: increment `suppressed`, drop the block, advance past the closing fence, set `in_fence = false`. If not match: emit the entire block (opening fence + body + closing fence), advance past the closing fence, set `in_fence = false`.
     - If not found: hold everything (still inside fence). Break.
3. Truncate `pending` to whatever is unconsumed.

**Algorithm — `flush`:**

- If `in_fence`: emit the unclosed fenced content as-is (we cannot tell if it would have been a tool block, and silently dropping is worse than showing a partial fence).
- Else: emit any tail bytes that were held back (max 2 chars).
- Reset state.

### 2. Register the module in [[../../../crates/agentos-kernel/src/lib.rs|lib.rs]]

Add `pub mod output_sanitizer;` next to the other top-level module declarations.

### 3. Wire into `chat_infer_streaming` in [[../../../crates/agentos-kernel/src/kernel.rs|kernel.rs]]

**Streaming filter — line 1454:**

Construct an `OutputSanitizerStream` once before the iteration loop (so state persists across iterations within a single chat turn), and apply it to each `Token` chunk:

```rust
let mut sanitizer = crate::output_sanitizer::OutputSanitizerStream::new();

while let Some(event) = inner_rx.recv().await {
    match event {
        agentos_llm::InferenceEvent::Token(chunk) => {
            let cleaned = sanitizer.push(&chunk);
            if !cleaned.is_empty() {
                let _ = tx.send(ChatStreamEvent::TextChunk { text: cleaned }).await;
            }
        }
        agentos_llm::InferenceEvent::Done(result) => {
            // Flush remaining buffered content as a final chunk before processing.
            let tail = sanitizer.flush();
            if !tail.is_empty() {
                let _ = tx.send(ChatStreamEvent::TextChunk { text: tail }).await;
            }
            inference_result = Some(result);
            break;
        }
        agentos_llm::InferenceEvent::Error(msg) => {
            stream_error = Some(msg);
            break;
        }
        _ => {}
    }
}
```

**Post-stream extractor — line 1487:**

After unwrapping `inference_result`, run the complete-text extractor on `result.text`. Always strip leaked blocks from the visible text (defense in depth). Promote extracted intents to `result.tool_calls` only when the adapter returned no native tool calls (avoids double-execution).

```rust
let result = match inference_result {
    Some(r) => r,
    None => { ... }
};

let extraction = crate::output_sanitizer::extract_tool_intent_blocks(&result.text);
let mut result = result;
if !extraction.extracted.is_empty() {
    tracing::warn!(
        target: "agentos::chat",
        agent = %agent_name,
        iteration = iterations,
        extracted = extraction.extracted.len(),
        adapter_native_count = result.tool_calls.len(),
        "Promoted leaked fenced tool-intent blocks to structured tool calls"
    );
    if result.tool_calls.is_empty() {
        for intent in extraction.extracted {
            result.tool_calls.push(agentos_llm::InferenceToolCall {
                id: None,
                tool_name: intent.tool,
                intent_type: intent.intent_type,
                payload: intent.payload,
            });
        }
    }
}
result.text = extraction.cleaned_text;
```

The rest of the iteration loop uses `result.text` and `result.tool_calls` unchanged, so the substituted/cleaned values flow through naturally.

### 4. Files changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/output_sanitizer.rs` | New module — `extract_tool_intent_blocks`, `OutputSanitizerStream`, unit tests |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod output_sanitizer;` |
| `crates/agentos-kernel/src/kernel.rs` | Wire sanitizer into `chat_infer_streaming` per-chunk filter and post-stream extractor (~30 lines) |

Total: 1 new file, 2 edits.

### 5. Dependencies

- Requires: nothing — pure addition.
- Blocks: Phase 2 (`<final>` enforcement reuses the streaming buffer pattern), Phase 3 (XML tool tag stripper extends the same module).

### 6. Test plan

Unit tests in `output_sanitizer.rs`:

| Test | Input | Expected |
|---|---|---|
| `extracts_single_tool_intent_block` | Text with one ` ```json ` block matching shape | Block removed, one extracted intent, surrounding prose preserved |
| `extracts_multi_intent_block` | One fenced block containing three concatenated JSON objects | Block removed, three extracted intents in order |
| `leaves_non_tool_json_alone` | ` ```json {"foo": 1} ``` ` | Block kept, no extractions |
| `leaves_mixed_block_alone` | One block containing one tool intent + one non-intent JSON | Block kept (mixed content is safer untouched), no extractions |
| `handles_no_fences` | Plain prose | Unchanged, no extractions |
| `handles_unclosed_fence` | Prose followed by ` ```json {"tool": "x"... ` (no closing fence) | Block kept (cannot be sure of intent without close), no extractions |
| `stream_filter_single_chunk` | Push entire text in one chunk | Same output as `extract_tool_intent_blocks` minus extraction (stream filter doesn't execute, only hides) |
| `stream_filter_chunk_split_mid_fence` | Split chunk inside the fence body | Block correctly held until closing fence arrives, then suppressed |
| `stream_filter_chunk_split_in_opening_fence` | Split chunk between two of the three opening backticks | Buffer holds correctly, no premature emit |
| `stream_filter_chunk_split_in_closing_fence` | Same for closing fence | Buffer holds correctly |
| `stream_filter_unclosed_fence_flushes` | Open fence, no close, flush() called | Buffered content emitted as-is |
| `stream_filter_non_tool_json_passthrough` | Stream a ` ```json {"foo": 1} ``` ` block | Block emitted unchanged |
| `stream_filter_unicode_in_fence_body` | Multibyte UTF-8 chars inside the fence body | No char-boundary panic, correct output |
| `stream_filter_suppression_count` | Three tool intent blocks streamed | `suppressed_count() == 3` |

### 7. Verification

```bash
# Build clean
cargo build -p agentos-kernel

# Module unit tests
cargo test -p agentos-kernel output_sanitizer

# Full kernel test suite (chat_infer_streaming integration tests should still pass)
cargo test -p agentos-kernel

# Lint clean
cargo clippy -p agentos-kernel -- -D warnings

# Format clean
cargo fmt --all -- --check
```

Expected: all four pass with the new module.

Manual verification:

1. Start the kernel and web UI.
2. Send a chat message that the model responds to with a fenced ` ```json ` tool intent block (e.g., ask "demonstrate calling a tool").
3. Observe the chat UI: the JSON block should NOT appear in the assistant's visible message.
4. Check kernel logs for `tracing::warn!` "Promoted leaked fenced tool-intent blocks" — confirms the extractor fired.
5. Check the audit log for the corresponding tool execution — confirms the extracted intent was actually run.

## Related

- [[Output Sanitization Plan]]
- [[Output Sanitization Research]]
- [[02-final-tag-enforcement]] — next phase
