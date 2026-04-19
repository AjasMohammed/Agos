---
title: Phase 4 — Code Region Awareness
tags:
  - kernel
  - chat
  - llm
  - security
  - phase
date: 2026-04-11
status: in-progress
effort: 0.5d
priority: medium
---

# Phase 4 — Code Region Awareness

> Let tutorial agents show literal tool-call syntax inside fenced code blocks without Phase 3's XML stripper deleting it. Mirrors OpenClaw's `findCodeRegions` + `isInsideCode` pattern.

---

## Why this phase

After Phases 1-3 ship, an agent trying to teach a user about AgentOS tool syntax would write something like:

````markdown
Here's how to call a tool in AgentOS. You emit a fenced JSON block:

```json
{"tool": "web-search", "intent_type": "query", "payload": {"query": "rust"}}
```

Or in XML form (some fine-tuned models use this):

```xml
<tool_call>
{"name": "web-search"}
</tool_call>
```

Got it?
````

Phase 1 already leaves the JSON example alone — `extract_tool_intent_blocks` only strips fenced ` ```json ` blocks that match the strict tool-intent shape, and this one does, so it **would** be stripped (and worse, executed). That's a fundamental design choice: without semantic markers like `<final>`, the sanitizer can't distinguish a real tool call from an example of one. Phase 2's `<final>` enforcement is the intended answer for tutorial agents; we leave `extract_tool_intent_blocks` alone.

Phase 3's `strip_xml_tool_tags`, however, is dumb regex-level stripping. It will delete the `<tool_call>...</tool_call>` example inside the ` ```xml ` block. That's wrong and fixable: skip tags whose opening `<` is inside a fenced code region.

Phase 2's `FinalTagFilter` already has fenced-code awareness via its `in_code_fence` state, so no change needed there. `extract_tool_intent_blocks` is out of scope for the reasons above.

## Current → target state

### Current (after Phase 3)

```text
"Example:\n```xml\n<tool_call>body</tool_call>\n```\nDone."
   → strip_xml_tool_tags →
"Example:\n```xml\n\n```\nDone."
```

The `<tool_call>` example is removed even though it's inside a code block.

### Target

```text
"Example:\n```xml\n<tool_call>body</tool_call>\n```\nDone."
   → strip_xml_tool_tags (code-region aware) →
"Example:\n```xml\n<tool_call>body</tool_call>\n```\nDone."
```

Unchanged — the code region is skipped.

A `<tool_call>` tag that actually leaks outside any code block is still stripped:

```text
"Let me call it.\n<tool_call>body</tool_call>\nDone."
   → strip_xml_tool_tags (code-region aware) →
"Let me call it.\n\nDone."
```

## Detailed subtasks

### 1. Add `find_fenced_code_regions` to [[../../../crates/agentos-kernel/src/output_sanitizer.rs|output_sanitizer.rs]]

```rust
/// Find the byte ranges inside fenced code blocks (` ``` `) in a complete
/// text. Returns one range per matching `code-fence-open ... code-fence-close`
/// pair. Unclosed fences extend to end-of-input.
///
/// The returned ranges cover the *body* of each block — the bytes between
/// the end of the opening fence's line and the start of the closing fence.
/// Opening language tags (` ```rust `) are part of the fence opening line
/// and are **not** inside the returned range; fence delimiter bytes
/// themselves are also outside the range.
fn find_fenced_code_regions(text: &str) -> Vec<std::ops::Range<usize>>;
```

Algorithm:

1. Walk `text` for triple backticks using the existing `find_triple_backtick` helper.
2. On the first ` ``` `, consume any language tag until the newline; record the next byte as `body_start`.
3. Find the next ` ``` `. If found, record `body_start..close_pos` as a region. Continue from `close_pos + 3`.
4. If not found, record `body_start..text.len()` as a region (unclosed fence) and stop.

### 2. Add `is_in_regions` helper

```rust
fn is_in_regions(pos: usize, regions: &[std::ops::Range<usize>]) -> bool {
    regions.iter().any(|r| r.contains(&pos))
}
```

For ~10 or fewer regions this O(n) linear scan is fine; a sorted-index binary search would be premature optimization.

### 3. Integrate into `strip_xml_tool_tags`

Compute `regions` once at the start. In the outer loop, after finding the next `<`, check if its position is inside a region. If yes, emit the `<` as plain text and advance one byte (exactly like the orphan-close-tag path).

Also integrate into `find_matching_close` so that a matching `</tool_call>` inside a nested code fence is not counted as the close — otherwise Phase 3 could be confused by a real `<tool_call>` whose body legitimately contains a code example showing ` ```xml<tool_call>nested</tool_call>```  `. (In practice this is rare, but correct.)

Actually — stepping back — `find_matching_close` only operates inside the body of an already-identified tool-call block. If the body contains a code fence with another `<tool_call>`, that nested example is **still inside the outer tool call** and gets stripped anyway. Code-region awareness at the inner level would cause the outer tool call to match its close tag incorrectly. Leaving `find_matching_close` alone is correct.

### 4. Keep `extract_tool_intent_blocks` and `FinalTagFilter` as-is

- `extract_tool_intent_blocks` — leaving alone because the conservative "must parse as strict tool intent JSON" rule already filters most tutorial content. A tutorial showing the literal AgentOS intent shape is indistinguishable from a real tool call without semantic markers; Phase 2 `<final>` enforcement is the answer for that scenario.
- `FinalTagFilter` — already has `in_code_fence` state; no change.

### 5. Test plan

New unit tests:

| Test | What it covers |
|---|---|
| `strip_xml_preserves_tool_call_inside_fenced_block` | `"Example: ```xml\n<tool_call>body</tool_call>\n```"` → unchanged |
| `strip_xml_strips_tool_call_outside_any_fence` | Regression: existing behavior preserved |
| `strip_xml_mixed_leak_and_example` | One real leaked `<tool_call>` + one inside a fenced block → only the leak is stripped |
| `strip_xml_unclosed_fence_protects_rest_of_text` | Unclosed ` ``` ` before a leaked `<tool_call>` → the leak is preserved because it falls inside the (implicit-to-EOF) fence region |
| `find_fenced_code_regions_single_closed_block` | One region returned for `"pre ```\nbody\n``` post"` |
| `find_fenced_code_regions_unclosed_block_extends_to_eof` | `"pre ```\nbody"` → region from after the fence to EOF |
| `find_fenced_code_regions_multiple_blocks` | Two regions returned, non-overlapping |
| `find_fenced_code_regions_no_fences` | Empty vec |
| `find_fenced_code_regions_handles_language_tag` | Language tag byte is outside the returned range |

### 6. Files changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/output_sanitizer.rs` | Add `find_fenced_code_regions`, `is_in_regions`, wire into `strip_xml_tool_tags`, ~9 new tests |

Total: 1 edit, no new files.

### 7. Dependencies

- Requires: Phase 3 (the function being extended).
- No downstream blockers.

### 8. Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel --lib output_sanitizer::
cargo test -p agentos-kernel --test e2e chat_tool_loop
cargo clippy -p agentos-kernel -p agentos-audit -- -D warnings
cargo fmt -p agentos-kernel -- --check
```

Manual verification: send a chat message asking the agent to explain AgentOS tool syntax. With Phase 3 alone, the example would be mangled. With Phase 4, it should render cleanly inside the code block.

## Related

- [[Output Sanitization Plan]]
- [[Output Sanitization Research]] — OpenClaw's `findCodeRegions` pattern
- [[03-xml-tool-tag-stripper]] — previous phase
- [[05-error-payload-rewriting]] — next phase
