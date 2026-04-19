---
title: Phase 3 — XML Tool Tag Stripper
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

# Phase 3 — XML Tool Tag Stripper

> Strip `<tool_call>`, `<tool_result>`, `<function_call>`, `<function_calls>`, `<tool_calls>`, `<invoke>`, and `<minimax:tool_call>` tags from LLM output when the model emits them as plain text instead of using the structured tool-use channel. Mirrors OpenClaw's `stripToolCallXmlTags` + `stripMinimaxToolCallXml` passes.

---

## Why this phase

Phases 1-2 close two leak paths:

- **Phase 1:** fenced ` ```json ` blocks matching the AgentOS intent shape
- **Phase 2:** free-form prose outside `<final>` (opt-in)

But many models trained on Claude/Anthropic conventions emit tool calls with XML syntax:

```text
Let me search for that.
<tool_call>
{"name": "web-search", "arguments": {"query": "..."}}
</tool_call>
```

Or the "invoke" style used by some fine-tunes:

```text
<function_calls>
<invoke name="web-search">
<parameter name="query">...</parameter>
</invoke>
</function_calls>
```

Or Minimax's particular wrapper:

```text
<minimax:tool_call>
<invoke name="...">...</invoke>
</minimax:tool_call>
```

When Phase 2 is **off** (the default), Phase 1 doesn't touch these tags and they leak to the UI. When Phase 2 is **on** and the model correctly wraps its user-facing reply in `<final>`, these leaks are already suppressed as "outside-`<final>` content." But most deployments will leave Phase 2 off (it requires training the model to use the convention), so Phase 3 provides a filter that works regardless of Phase 2 state.

OpenClaw uses a bespoke `stripToolCallXmlTags` function ([assistant-visible-text.ts:168-260](https://github.com/openclaw/openclaw/blob/main/src/shared/text/assistant-visible-text.ts#L168-L260)) that's stateful, supports truncated tags, and preserves nesting balance. We'll take a simpler approach since the kernel already has structured tool-call extraction for the fenced-JSON case and we only need to strip the tags as visible noise — no need to re-promote XML-formatted intents into `result.tool_calls` (that's an explicit non-goal: we rely on the LLM adapter's structured tool-use channel for actual execution).

## Current → target state

### Current (after Phase 2)

```text
result.text:
  "Let me search.\n<tool_call>\n{\"name\":\"x\"}\n</tool_call>\nDone."

visible_text (Phase 2 off):
  "Let me search.\n<tool_call>\n{\"name\":\"x\"}\n</tool_call>\nDone."   ← leaks
```

### Target

```text
result.text:
  "Let me search.\n<tool_call>\n{\"name\":\"x\"}\n</tool_call>\nDone."

visible_text (Phase 3 on, Phase 2 off):
  "Let me search.\n\nDone."
```

Behavior: strip opening-tag-through-closing-tag blocks for known tool-call XML tag names. Leave the surrounding prose intact. If an opening tag is unclosed, strip everything from the tag to end-of-input.

## Detailed subtasks

### 1. Add `strip_xml_tool_tags` to [[../../../crates/agentos-kernel/src/output_sanitizer.rs|output_sanitizer.rs]]

Public API — a pure function, not a streaming filter:

```rust
/// Strip XML-style tool-call tags (and their bodies) from a complete text.
/// The recognized tag names match those emitted by popular LLM conventions:
///
/// - `<tool_call>...</tool_call>`
/// - `<tool_result>...</tool_result>`
/// - `<tool_calls>...</tool_calls>`
/// - `<function_call>...</function_call>`
/// - `<function_calls>...</function_calls>`
/// - `<invoke>...</invoke>` (used by function_calls-style payloads)
/// - `<minimax:tool_call>...</minimax:tool_call>` (Minimax-specific)
///
/// Matching is case-insensitive on the tag name and ignores XML attributes
/// on the opening tag (e.g., `<invoke name="search">`). Tag content up to
/// the matching closing tag is removed. Unclosed tags strip everything from
/// the opening tag to end-of-input.
///
/// This function is a **text-only visible-output filter** — it does not try
/// to re-promote stripped XML-formatted tool calls into `result.tool_calls`.
/// The kernel's structured tool-use channel is the source of truth for
/// actual execution.
pub fn strip_xml_tool_tags(text: &str) -> String { ... }
```

### 2. Algorithm

For each recognized tag name `T`:

1. Walk `text` byte-by-byte looking for `<T` (case-insensitive, followed by `>`, whitespace, or `/`).
2. When found, advance to the next `>` to find the end of the opening tag.
3. Search for the matching `</T>` (case-insensitive), accounting for nested `<T>` occurrences with a depth counter.
4. Remove bytes from the start of the opening tag through the end of the closing tag (or to end-of-input for unclosed tags).
5. Repeat until no more opening tags found.

Implementation will be O(n × k) where k = number of tag names; since k is small (~7) and n is small (~2KB), this is fine.

**ASCII-only scan:** tag names are ASCII. All byte positions touched by the scanner are ASCII boundaries, which are always char boundaries.

**Attributes on opening tag:** permitted. `<invoke name="search" id="call_1">` is handled by finding the next `>` and slicing from there.

**Self-closing tags:** `<invoke/>` — treat as a no-content match, strip from `<` to `/>`.

**Case sensitivity:** match tag names case-insensitively. The body content is untouched (preserving original casing is irrelevant since we're deleting it).

### 3. Integrate into `sanitize_chat_inference_result`

After the fenced-block extraction in [[../../../crates/agentos-kernel/src/kernel.rs|kernel.rs]], and before the optional `<final>` filter step, run `strip_xml_tool_tags` on both `result.text` and — implicitly, by re-using the cleaned form — what becomes `visible_text`.

Preserve Phase 2's invariant: `result.text` retains the model's raw reasoning for the context window, but the XML tool tags ARE stripped from `result.text` too (just like fenced tool blocks are). Reason: XML tool tags in the persisted assistant context would re-tempt the model to repeat the leak format on the next turn.

```rust
let extraction = crate::output_sanitizer::extract_tool_intent_blocks(&result.text);
// ... existing audit + promote logic ...
result.text = crate::output_sanitizer::strip_xml_tool_tags(&extraction.cleaned_text);
```

### 4. Test plan

Unit tests in `output_sanitizer.rs`:

| Test | Input | Expected |
|---|---|---|
| `strip_xml_single_tool_call_tag` | `"Before.<tool_call>{}</tool_call>After."` | `"Before.After."` |
| `strip_xml_tool_result_tag` | With `<tool_result>` | Removed |
| `strip_xml_function_call_and_calls` | `<function_call>`, `<function_calls>` | Both removed |
| `strip_xml_invoke_tag` | `<invoke name="x"><parameter>1</parameter></invoke>` | Removed |
| `strip_xml_minimax_tool_call` | `<minimax:tool_call>...</minimax:tool_call>` | Removed |
| `strip_xml_case_insensitive` | `<ToolCall>...</toolcall>` | Removed |
| `strip_xml_with_attributes` | `<tool_call id="call_1" lang="en">...</tool_call>` | Removed |
| `strip_xml_unclosed_tag_strips_to_eof` | `before<tool_call>oops` | `"before"` |
| `strip_xml_nested_same_tag` | `<tool_call>outer<tool_call>inner</tool_call>rest</tool_call>` | Fully removed (balanced depth) |
| `strip_xml_multiple_separate_blocks` | Two tool_call blocks with prose between | Both removed, prose preserved |
| `strip_xml_leaves_unrelated_tags_alone` | `<em>foo</em><final>bar</final>` | Unchanged — not in the known list |
| `strip_xml_ignores_case_in_close_tag_too` | `<tool_call>x</TOOL_CALL>` | Removed |
| `strip_xml_self_closing_invoke` | `<invoke name="ping"/>` | Removed |
| `strip_xml_empty_string` | `""` | `""` |
| `strip_xml_no_tags` | `"plain prose"` | unchanged |
| `sanitize_chat_result_strips_xml_tool_call_in_visible_text` | Full `InferenceResult` with `<tool_call>` in text | `visible_text` has no tag; `result.text` also cleaned |

### 5. Files changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/output_sanitizer.rs` | Add `strip_xml_tool_tags` + `KNOWN_TOOL_TAG_NAMES` const + ~15 unit tests |
| `crates/agentos-kernel/src/kernel.rs` | Wire `strip_xml_tool_tags` into `sanitize_chat_inference_result` after fenced-block extraction |

Total: 2 edits, no new files.

### 6. Dependencies

- Requires: Phase 1 (`extract_tool_intent_blocks`) for clean ordering.
- Complementary to: Phase 2 (`<final>` enforcement) — both paths work independently.
- Blocks: Phase 4 (code-region awareness needs the same tag detector to skip stripping inside fenced code blocks).

### 7. Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel --lib output_sanitizer::
cargo test -p agentos-kernel --test e2e chat_tool_loop
cargo clippy --workspace -- -D warnings
cargo fmt -p agentos-kernel -- --check
```

Manual verification:

1. Configure an agent with a model prone to emitting `<tool_call>` XML (e.g., locally-hosted Llama fine-tunes).
2. Send a chat that would trigger a tool call.
3. Observe: the chat UI no longer shows `<tool_call>` tags. The tool still runs if the adapter populates the structured channel; it doesn't run if the model only used XML syntax (that's expected — Phase 3 is a visible-text filter, not an execution bridge).
4. Check kernel logs for `ToolIntentLeakedFromText` audit events — they will NOT fire for XML leaks in this phase (out of scope).

## Related

- [[Output Sanitization Plan]]
- [[Output Sanitization Research]]
- [[02-final-tag-enforcement]] — previous phase
- [[04-code-region-awareness]] — next phase
