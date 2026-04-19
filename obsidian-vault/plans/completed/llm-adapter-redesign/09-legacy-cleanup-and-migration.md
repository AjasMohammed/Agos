---
title: "Phase 9: Legacy Cleanup and Migration Completion"
tags:
  - llm
  - kernel
  - v3
  - plan
date: 2026-03-24
status: complete
effort: 1.5d
priority: medium
---

# Phase 9: Legacy Cleanup and Migration Completion

> Remove the legacy text-based tool call format (`append_legacy_blocks`, `render_legacy_tool_blocks`), clean up unused code paths, and ensure all kernel code paths use the native `InferenceToolCall` pipeline exclusively.

---

## Why This Phase

After Phases 1-8, the native tool call pipeline is fully operational. But the legacy text-based pipeline still exists:

1. `tool_helpers.rs` still has `render_legacy_tool_blocks()` and `append_legacy_blocks()` which render `InferenceToolCall` structs into markdown ````json` blocks.
2. Each adapter's response parser still calls `append_legacy_blocks()` to produce dual-format output.
3. The kernel's `tool_call.rs` still has `parse_tool_call()` and `parse_tool_calls()` regex parsers.
4. The kernel `chat_infer_with_tools` and `execute_task_sync` still have legacy fallback branches.

This phase removes the legacy pipeline, making native tool calls the only path. This simplifies the codebase and eliminates the round-trip text rendering/parsing that loses information.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `tool_helpers::render_legacy_tool_blocks` | Used by all adapters | Removed |
| `tool_helpers::append_legacy_blocks` | Used by all adapters | Removed |
| `InferenceResult.text` when tools present | Contains both reasoning text AND rendered JSON blocks | Contains only reasoning text (tool calls are in `tool_calls` field) |
| Kernel `parse_tool_call()` | Used as primary detection in chat loop | Removed (or moved behind feature flag for backward compat) |
| Kernel `parse_tool_calls()` | Used in task executor | Removed (or feature-flagged) |
| Chat loop | Native-first, legacy-fallback | Native-only |
| Task executor loop | Native-first, legacy-fallback | Native-only |

---

## What to Do

### Step 1: Audit all call sites of legacy functions

Search for all usages to ensure nothing is missed:

```bash
cargo grep 'append_legacy_blocks\|render_legacy_tool_blocks\|parse_tool_call\b\|parse_tool_calls\b'
```

Expected call sites:
- `crates/agentos-llm/src/openai.rs` -- `append_legacy_blocks`
- `crates/agentos-llm/src/anthropic.rs` -- `append_legacy_blocks`
- `crates/agentos-llm/src/gemini.rs` -- `append_legacy_blocks`
- `crates/agentos-llm/src/tool_helpers.rs` -- definitions
- `crates/agentos-kernel/src/kernel.rs` -- `parse_tool_call`
- `crates/agentos-kernel/src/task_executor.rs` -- `parse_tool_calls`
- `crates/agentos-kernel/src/tool_call.rs` -- definitions

### Step 2: Remove `append_legacy_blocks` calls from adapters

In `openai.rs`, `anthropic.rs`, and `gemini.rs`, find the line:
```rust
let text = tool_helpers::append_legacy_blocks(&text, &tool_calls);
```
Replace with:
```rust
// Tool calls are conveyed via InferenceResult.tool_calls, not rendered into text.
```

This means `InferenceResult.text` will only contain the model's reasoning/text output, not the rendered JSON blocks. This is the correct behavior -- the kernel reads `result.tool_calls` directly.

### Step 3: Remove legacy functions from `tool_helpers.rs`

Remove `render_legacy_tool_blocks()` and `append_legacy_blocks()`. Keep `infer_intent_type_from_permissions()`, `normalize_tool_input_schema()`, `check_payload_size()`, and `validate_payload_object()` -- these are still useful.

Remove the associated tests for the removed functions.

### Step 4: Remove legacy fallback from kernel `chat_infer_with_tools`

In `crates/agentos-kernel/src/kernel.rs`, the loop now only checks:
```rust
if result.stop_reason == StopReason::ToolUse && !result.tool_calls.is_empty() {
    // Execute native tool calls
} else {
    // Final answer
    break result.text;
}
```

Remove the `parse_tool_call(&result.text)` fallback branch entirely.

### Step 5: Remove legacy fallback from kernel `chat_infer_streaming`

Same change as Step 4 for the streaming variant.

### Step 6: Remove legacy fallback from `execute_task_sync`

In `crates/agentos-kernel/src/task_executor.rs`, remove the `parse_tool_calls(&inference.text)` fallback. Use only `inference.tool_calls` with `inference.stop_reason` check.

### Step 7: Deprecate or remove `tool_call.rs` parsing functions

If `parse_tool_call()` and `parse_tool_calls()` have no remaining callers, remove them. If other code (e.g., tests, pipeline) still uses them, mark them `#[deprecated]` with a note to use native tool calls.

### Step 8: Run full test suite and fix fallout

After removing legacy code, some tests may construct `InferenceResult` with text containing JSON blocks and expect the kernel to parse them. These tests need updating to use `MockResponse::with_tool_calls()` instead.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/tool_helpers.rs` | Remove `render_legacy_tool_blocks`, `append_legacy_blocks`, associated tests |
| `crates/agentos-llm/src/openai.rs` | Remove `append_legacy_blocks` call from response parsing |
| `crates/agentos-llm/src/anthropic.rs` | Remove `append_legacy_blocks` call |
| `crates/agentos-llm/src/gemini.rs` | Remove `append_legacy_blocks` call |
| `crates/agentos-kernel/src/kernel.rs` | Remove legacy `parse_tool_call` fallback from chat loops |
| `crates/agentos-kernel/src/task_executor.rs` | Remove legacy `parse_tool_calls` fallback from task executor |
| `crates/agentos-kernel/src/tool_call.rs` | Remove or deprecate `parse_tool_call`, `parse_tool_calls` |

---

## Prerequisites

All prior phases (1-8) must be complete. Specifically:
- Phase 3: Kernel is already using `StopReason` + native tool calls (with legacy fallback)
- Phase 7: `MockLLMCore` supports tool calls and stop reasons, so tests can be migrated

---

## Test Plan

- `cargo build --workspace` must pass with zero errors
- `cargo test --workspace` must pass -- this is the critical gate
- Verify no remaining references to `parse_tool_call` or `append_legacy_blocks` in the codebase (except `#[deprecated]` markers if retained)
- Run the web UI chat manually (if available) to confirm tool-using conversations still work end-to-end
- Verify `InferenceResult.text` no longer contains ````json` tool call blocks when tool calls are present

---

## Verification

```bash
cargo build --workspace
cargo test --workspace -- --nocapture
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
# Verify no legacy references remain:
grep -rn 'append_legacy_blocks\|render_legacy_tool_blocks' crates/ --include='*.rs' | grep -v '#\[deprecated\]' | grep -v '// removed'
```
