---
title: Phase 2 — Final Tag Enforcement
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

# Phase 2 — `<final>` Tag Enforcement

> Add an opt-in strict mode where only text inside `<final>...</final>` blocks is shown to the user, plus a `<think>...</think>` stripper that always runs. Catches arbitrary "thinking out loud" leaks Phase 1's fenced-block stripper cannot.

---

## Why this phase

Phase 1 closes the fenced-` ```json ` tool-block leak path. But there are *other* leak paths it cannot catch:

- The model writes plain prose like `"Let me think about this... actually I'll call the search tool now..."` and then proceeds to call the tool. The "thinking out loud" prose leaks.
- The model wraps an internal reasoning step in `<think>...</think>` tags (a convention some models use). The tags pass through as visible text and confuse the user.
- The model emits a tool call as a non-fenced JSON object (`{"tool":"x"...}` without backticks). Phase 1 doesn't catch unfenced JSON.
- The model emits an XML-style tool call (`<tool_call>...</tool_call>`). Phase 3 will handle that, but Phase 2's `<final>` enforcement catches it incidentally because it's outside any `<final>` block.

OpenClaw uses an explicit `<final>...</final>` enforcement gate (see [[Output Sanitization Research]] Layer B): in strict mode, **only text inside `<final>...</final>` reaches the user**. Everything else is dropped. The system prompt instructs the model to wrap its visible answer in the tags. This gives operators a hard guarantee that nothing leaks unless the model explicitly marks it as user-facing.

Same approach here, opt-in via config so existing deployments are unaffected.

## Current → target state

### Current (after Phase 1)

```text
LLM stream → OutputSanitizerStream → SSE
                ↑ strips ```json tool blocks
```

Plain prose between/around tool blocks still flows through verbatim.

### Target

```text
LLM stream → OutputSanitizerStream → FinalTagFilter (opt-in) → SSE
                ↑                       ↑
                strips ```json blocks   strips <think>; in strict mode
                                         drops content outside <final>...</final>
```

Composed via a new `ChatOutputFilter` wrapper so the kernel call sites stay simple.

## Detailed subtasks

### 1. Add `ChatConfig` to kernel config

[[../../../crates/agentos-kernel/src/config.rs|config.rs]] — add a new section:

```rust
/// Chat-related kernel configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ChatConfig {
    /// When true, the chat output filter only emits text appearing inside
    /// `<final>...</final>` tags. The system prompt is updated to instruct
    /// the model to wrap its visible answer in those tags. Off by default —
    /// flipping this on is a behavioral change that requires the connected
    /// LLM to follow the convention or the user gets a placeholder reply.
    #[serde(default)]
    pub enforce_final_tag: bool,
}
```

Add `#[serde(default)] pub chat: ChatConfig,` to `KernelConfig`.

### 2. Implement `FinalTagFilter` in [[../../../crates/agentos-kernel/src/output_sanitizer.rs|output_sanitizer.rs]]

Public API:

```rust
pub struct FinalTagFilter {
    in_final: bool,
    ever_in_final: bool,
    in_think: bool,
    in_code_fence: bool,
    pending: String,
}

impl FinalTagFilter {
    pub fn new() -> Self;
    pub fn push(&mut self, chunk: &str) -> String;
    pub fn flush(&mut self) -> String;
    pub fn ever_in_final(&self) -> bool;
}
```

State machine:

| Tag | When seen outside code fence | Effect |
|---|---|---|
| `<final>` | sets `in_final = true`, `ever_in_final = true` | drops the tag itself, starts emitting subsequent text |
| `</final>` | sets `in_final = false` | drops the tag, stops emitting |
| `<think>` | sets `in_think = true` | drops the tag, suppresses subsequent text even when in_final |
| `</think>` | sets `in_think = false` | drops the tag, resumes emitting if in_final |
| ` ``` ` (triple backtick) | toggles `in_code_fence` | always emitted as text |
| any other `<…>` | not a recognized tag | passed through as text |

**Emission rule:** A character is emitted iff `in_final && !in_think`. Code fence state does not gate emission — code blocks inside `<final>` are emitted; code blocks outside `<final>` are not.

**Code-fence awareness:** While `in_code_fence` is true, `<final>` and `<think>` are *not* recognized as tags — they pass through as plain text. This lets a model show literal `<final>` example syntax inside ` ```html ` tutorial blocks.

**Chunk-boundary handling:** when a `<` is found near the end of a chunk and the bytes that follow are a prefix of any recognized tag (`<final>`, `</final>`, `<think>`, `</think>`), buffer everything from the `<` and retry on the next push. Same for trailing partial backticks (1-2 of them).

### 3. Add `ChatOutputFilter` composer

```rust
pub struct ChatOutputFilter {
    sanitizer: OutputSanitizerStream,
    final_filter: Option<FinalTagFilter>,
}

impl ChatOutputFilter {
    pub fn new(enforce_final_tag: bool) -> Self;
    pub fn push(&mut self, chunk: &str) -> String;
    pub fn flush(&mut self) -> String;
    pub fn suppressed_block_count(&self) -> usize;
    /// `true` if the stream contained at least one `<final>` open tag, OR
    /// final-tag enforcement is disabled. Used by the kernel to decide
    /// whether to substitute `EMPTY_LLM_ANSWER_PLACEHOLDER`.
    pub fn had_final_tag(&self) -> bool;
}
```

Composition: chunks flow through the existing `OutputSanitizerStream` first, then (when present) through `FinalTagFilter`. This ordering matters — we want fenced JSON tool blocks stripped *before* `<final>` enforcement so the JSON contents don't get misinterpreted as tag content.

### 4. Update system prompt builder

[[../../../crates/agentos-kernel/src/system_prompt.rs|system_prompt.rs]] — add an `enforce_final_tag: bool` field to `SystemPromptContext`. When true, append a section before `## Tools`:

```text
## Output Format
Wrap your final user-facing answer in `<final>...</final>` tags. Anything
outside `<final>` blocks is hidden from the user. Use `<think>...</think>`
for internal reasoning that should not be shown.

Example:
<think>Let me check the current weather first.</think>
[tool call]
<final>The weather in Tokyo is 18°C and clear.</final>
```

When false, no addition (current behavior preserved).

### 5. Wire `ChatOutputFilter` into both chat paths

In `chat_infer_streaming`:

- Replace `let mut sanitizer = OutputSanitizerStream::new();` with `let mut filter = ChatOutputFilter::new(self.config.chat.enforce_final_tag);`
- Update token loop to call `filter.push(&chunk)` and `filter.flush()`.
- After receiving the result, if `!filter.had_final_tag()` and the cleaned text is empty/whitespace, substitute `EMPTY_LLM_ANSWER_PLACEHOLDER` (existing fallback at line 1750+ already handles empty text — no extra code needed, but verify).

In `chat_infer_with_tools` (non-streaming): apply the same `ChatOutputFilter` to `result.text` in one shot via `filter.push(&result.text)` + `filter.flush()`. Reuse the existing `sanitize_chat_inference_result` helper by extending it to take a `ChatOutputFilter` parameter, or add a sister helper.

In both paths, also pass `self.config.chat.enforce_final_tag` into `build_system_prompt` so the model is instructed to use the convention.

### 6. Test plan

Unit tests in `output_sanitizer.rs` (extending the existing `tests` module):

| Test | What it covers |
|---|---|
| `final_filter_passes_through_when_no_tags` | Strict-off behavior — everything emitted |
| `final_filter_drops_outside_text_in_strict_mode` | "thinking" + `<final>real</final>` → only `real` emitted |
| `final_filter_strips_think_blocks_inside_final` | `<final>a<think>secret</think>b</final>` → `ab` |
| `final_filter_strips_think_blocks_outside_final_too` | `<think>secret</think><final>a</final>` → `a` (think content suppressed regardless) |
| `final_filter_handles_chunk_split_inside_open_tag` | `<fi` then `nal>...` → tag recognized after merge |
| `final_filter_handles_chunk_split_inside_close_tag` | `</fi` then `nal>...` |
| `final_filter_passes_through_inside_code_fence` | `<final>before` + code block containing literal `<final>` → inner tags treated as text |
| `final_filter_unknown_tags_passed_through` | `<final><user_data>foo</user_data></final>` → `<user_data>foo</user_data>` |
| `final_filter_emits_nothing_when_no_final_tag` | Plain prose with no `<final>` → empty output, `ever_in_final() == false` |
| `composite_chat_filter_strips_tool_block_then_enforces_final` | The exact user leak example wrapped in `<final>` outside the JSON block: tool block stripped, prose preserved |
| `composite_chat_filter_strict_off_acts_like_phase_1` | When `enforce_final_tag = false`, behavior matches `OutputSanitizerStream` alone |
| `composite_chat_filter_had_final_tag_disabled_returns_true` | When strict is off, `had_final_tag()` always returns true so the kernel doesn't substitute the placeholder |

System prompt tests:

| Test | What it covers |
|---|---|
| `system_prompt_includes_final_tag_section_when_enforced` | `enforce_final_tag = true` → contains "## Output Format" and `<final>` example |
| `system_prompt_omits_final_tag_section_when_disabled` | Default → no `<final>` mention |

Kernel-config tests already cover the default-deserialization path through `#[serde(default)]`.

### 7. Files changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/config.rs` | Add `ChatConfig` struct, add to `KernelConfig` |
| `crates/agentos-kernel/src/output_sanitizer.rs` | Add `FinalTagFilter`, `ChatOutputFilter`, ~12 new tests |
| `crates/agentos-kernel/src/system_prompt.rs` | Add `enforce_final_tag` field, conditional section, 2 new tests |
| `crates/agentos-kernel/src/kernel.rs` | Wire `ChatOutputFilter` into both chat paths, pass `enforce_final_tag` to system prompt |

Total: 4 edits, no new files.

### 8. Dependencies

- Requires: Phase 1 (builds on `OutputSanitizerStream`).
- Blocks: Phase 4 (code-region awareness improvements may want to share the fence-tracking machinery added here).

### 9. Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel --lib output_sanitizer
cargo test -p agentos-kernel --lib system_prompt
cargo test -p agentos-kernel --test e2e chat_tool_loop
cargo clippy --workspace -- -D warnings
cargo fmt -p agentos-kernel -- --check
```

Manual verification with strict mode on:

1. Set `[chat] enforce_final_tag = true` in `config/default.toml`.
2. Restart the kernel.
3. Send a chat message that the model responds to with prose (no `<final>` tags). Expect the user to see the empty-answer placeholder — the model has not yet learned the convention.
4. Update the agent's system prompt at runtime (or wait for the next session) and confirm the model wraps its reply in `<final>...</final>`.
5. Send a chat that triggers a tool call. The reasoning between calls should be hidden; only the final summary inside `<final>` should appear.

## Related

- [[Output Sanitization Plan]]
- [[Output Sanitization Research]]
- [[01-fenced-tool-block-extractor]] — previous phase
- [[03-xml-tool-tag-stripper]] — next phase
