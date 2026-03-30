# Context Memory Hardening — Design Spec

**Date:** 2026-03-29
**Status:** Approved
**Scope:** Three targeted improvements to context window management for long-running agent tasks.

---

## Problem

On long-running tasks (20+ iterations), agents silently lose early context through compression and eviction. Three specific gaps:

1. **Summarization is concatenation, not summarization.** `compress_oldest()` truncates entries to 150 chars and concatenates them. This preserves entry count but not semantic content. Critical reasoning and tool outputs from early iterations are reduced to useless snippets.

2. **The agent doesn't know what it forgot.** No signal is injected into context when entries are compressed or evicted. The agent has no reason to call `memory-read` to recover lost details because it doesn't know anything is missing.

3. **`reference_count` is dead weight.** The field exists on `ContextEntry` and contributes 30% to `SemanticEviction` scoring, but is never incremented. All entries score 0.0 on this axis, making eviction less intelligent than designed.

---

## Solution Overview

Three independent features, implemented in dependency order:

| # | Feature | Summary |
|---|---------|---------|
| 1 | LLM-generated summarization | Replace concat compression with real LLM summarization, falling back to concat on failure |
| 2 | Context-loss notice | Inject a short system entry telling the agent what was compressed and how to recover it |
| 3 | Reference count tracking | Track which tool results the agent actively references and make them resist eviction |

---

## Feature 1: LLM-Generated Summarization

### Current Behavior

`ContextWindow::compress_oldest(count)` in `agentos-types/src/context.rs:260-316`:
- Iterates oldest entries, skips pinned/System
- Truncates each to 150 chars
- Creates a `[TOKEN BUDGET SUMMARY]` entry with `is_summary: true`, importance 0.3

Called from `ContextManager::push_entry()` in `agentos-kernel/src/context.rs:78-104`:
- At 80% token budget: compress `len/4` entries
- At 95% token budget: compress `len/3` entries + set `needs_checkpoint`

### Target Behavior

`ContextManager::push_entry()` orchestrates summarization:

1. When compression triggers (80% or 95%), call `ContextWindow::extract_compressible(count)` to remove and return the entries
2. If `config.context.summarization_mode == "llm"` and an LLM adapter is available:
   - Build a minimal `ContextWindow` with a summarization system prompt and the full text of extracted entries
   - Call the agent's LLM adapter via `infer()` with a focused prompt
   - On success: insert the LLM-generated summary via `ContextWindow::insert_summary_entry(content)`
   - On failure (timeout, error, no adapter): fall back to the concat path using the extracted entries
3. If mode is `"concat"`: use existing concat logic directly
4. Record cost against agent budget via `CostTracker::record_inference_with_cost()`

### Summarization Prompt

```
Summarize the following conversation messages into a concise paragraph.
Preserve: key decisions, tool outputs that produced important results, error messages, and any facts the agent discovered.
Discard: routine acknowledgments, redundant tool calls, and verbose formatting.
Keep the summary under 300 words.

Messages:
{entries formatted as [Role]: content}
```

### Configuration

New `[context]` section in `config/default.toml` and `ContextConfig` struct in `config.rs`:

```toml
[context]
# "llm" = LLM-generated summaries (best-effort, falls back to concat)
# "concat" = existing concatenation behavior
# "off" = no compression (entries evicted without summary)
summarization_mode = "llm"
# Maximum chars of entry text sent to the summarizer LLM per compression event.
# Prevents sending enormous payloads on aggressive compression passes.
summarization_max_input_chars = 8000
```

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextConfig {
    #[serde(default = "default_summarization_mode")]
    pub summarization_mode: SummarizationMode,
    #[serde(default = "default_summarization_max_input_chars")]
    pub summarization_max_input_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SummarizationMode {
    #[default]
    Llm,
    Concat,
    Off,
}
```

Defaults: `summarization_mode = "llm"`, `summarization_max_input_chars = 8000`.

### Architectural Constraint

`ContextWindow` lives in `agentos-types` (no kernel dependencies). LLM-aware summarization logic lives in `ContextManager` (kernel crate). The split:

- **`agentos-types::ContextWindow`** gains two new methods:
  - `extract_compressible(count: usize) -> Vec<ContextEntry>` — removes and returns up to `count` non-pinned, non-System entries from oldest first. Same selection logic as current `compress_oldest` but returns entries instead of concatenating.
  - `insert_summary_entry(content: String, compressed_count: usize)` — inserts a summary entry at the correct position (after System entries). Sets `is_summary: true`, importance 0.3, category History.

- **`agentos-kernel::ContextManager`** gains:
  - `summarize_entries_llm(entries: &[ContextEntry], llm: &dyn LLMCore, max_input_chars: usize) -> Result<String, anyhow::Error>` — builds a minimal context window, calls `llm.infer()`, returns the summary text.
  - `summarize_entries_concat(entries: &[ContextEntry]) -> String` — the existing concat logic, extracted as a standalone function for fallback.

- **`ContextManager` constructor** changes: needs access to `Arc<RwLock<HashMap<AgentID, Arc<dyn LLMCore>>>>` (the `active_llms` map), `Arc<CostTracker>`, and the `ContextConfig`. These are passed at construction time.

- **`push_entry` signature change**: needs `agent_id: &AgentID` parameter so it can look up the correct LLM adapter and record cost. This is a breaking change to the internal API. All call sites in `task_executor.rs` and `context_injector.rs` already have `agent_id` available.

### Cost Tracking

Each LLM summarization call is recorded via `CostTracker::record_inference_with_cost()` with:
- The agent's ID
- Token usage from the inference result
- Provider and model name from the adapter
- Pre-computed cost if available

This means summarization counts against the agent's budget. Operators can disable it via `summarization_mode = "concat"` for budget-constrained agents.

### Fallback Behavior

The LLM summarization is best-effort. Fallback to concat occurs when:
- `summarization_mode` is `"concat"`
- No LLM adapter is available for the agent (adapter lookup returns None)
- The LLM `infer()` call returns an error (timeout, rate limit, API error)
- The LLM returns an empty response

On fallback, a WARN-level tracing log is emitted and the existing concat logic runs. The agent gets a summary either way.

### `compress_oldest` Preservation

The existing `compress_oldest()` method on `ContextWindow` is kept as-is for backward compatibility (used by overflow strategies in `push()`). The new `extract_compressible` + `insert_summary_entry` pair is used only by the token-budget compression path in `ContextManager::push_entry()`.

---

## Feature 2: Context-Loss Notice

### Current Behavior

No signal to the agent when entries are compressed or evicted. Events (`ContextWindowNearLimit`, `ContextWindowExhausted`) are emitted to the monitoring system but not injected into the agent's context.

### Target Behavior

After each compression event in `push_entry()`, inject (or update) a short pinned System entry:

```
[CONTEXT NOTE] {N} earlier messages were compressed into a summary.
To recall specific details, use memory-read with scope=episodic and your current task ID.
```

### Behavior Rules

- **Inject once per compression event.** If a previous context-loss notice exists, update it in-place (replace content with updated count).
- **Pinned, importance 0.9.** Stays visible to the agent but below the main system prompt (1.0). Can never be evicted by SemanticEviction.
- **Category: System.** Included in the System category budget (15% of tokens).
- **Not marked `is_summary`.** This is a real system instruction, not a synthetic summary.
- **Content is ~50 tokens.** Negligible budget impact.
- **Identification:** The entry is identified by a sentinel prefix `[CONTEXT NOTE]` in its content. The `find_and_update_context_notice()` method scans for this prefix.

### Implementation

- `ContextWindow` gains `upsert_context_notice(compressed_count: usize)`:
  - Scans entries for one with content starting with `[CONTEXT NOTE]`
  - If found: update its content with the new count, reset timestamp
  - If not found: insert a new pinned System entry after the main system prompt

- Called from `ContextManager::push_entry()` immediately after the summary entry is inserted.

---

## Feature 3: Reference Count Tracking

### Current Behavior

`ContextEntry::reference_count` is defined, defaults to 0, and contributes 30% to the `evict_by_semantic_score()` composite. But it is never incremented anywhere in the codebase. Every entry always scores 0.0 on this axis.

### Target Behavior

After each assistant response is pushed to context, scan the response for tool call references. For each tool call whose `tool_call_id` matches an existing ToolResult entry in the context window, increment that entry's `reference_count`.

### Logic

Track references structurally via the multi-turn protocol:

- When a new ToolResult entry is pushed to context with a `tool_call_id`, find the Assistant entry whose `metadata.assistant_tool_calls` JSON contains that same `tool_call_id`. Increment `reference_count` on *both* the Assistant entry (it made the call) and the ToolResult entry (it's the response).
- This creates a bidirectional link: assistant entries that generated tool calls, and tool results that answered them, both get higher eviction resistance.
- This is precise, cheap (JSON field matching), and covers the primary case.

Called from `task_executor.rs` after tool results are pushed back into context. The `tool_call_id` values are already available from the parsed tool calls.

### Implementation

- `ContextWindow` gains `increment_references_for_tool_call_ids(ids: &[String])`:
  - For each ID, scan entries:
    - If a ToolResult entry has `metadata.tool_call_id == id`, increment its `reference_count`
    - If an Assistant entry has `metadata.assistant_tool_calls` containing the ID, increment its `reference_count`

- `ContextManager` gains `increment_references(task_id: &TaskID, tool_call_ids: &[String])`:
  - Acquires write lock, calls `window.increment_references_for_tool_call_ids(ids)`

- Called from `task_executor.rs` after tool results are pushed back into context. The tool_call_ids are already available from the parsed tool calls.

### Effect

With reference_count now populated:
- Tool results that are part of active multi-turn chains get `reference_count >= 1`
- Their SemanticEviction score gains +0.3 (normalized), making them significantly harder to evict
- Old tool results that were never part of a chain (or whose chain completed long ago) stay at 0 and are evicted first
- The 30% weight in SemanticEviction scoring is no longer dead weight

---

## Files Changed (Summary)

| File | Changes |
|------|---------|
| `config/default.toml` | Add `[context]` section |
| `crates/agentos-kernel/src/config.rs` | Add `ContextConfig`, `SummarizationMode`; add `context` field to `KernelConfig` |
| `crates/agentos-types/src/context.rs` | Add `extract_compressible()`, `insert_summary_entry()`, `upsert_context_notice()`, `increment_references_for_tool_call_ids()` |
| `crates/agentos-kernel/src/context.rs` | Refactor `ContextManager` constructor (add LLM/cost/config deps); refactor `push_entry()` for LLM summarization + notice; add `increment_references()`, `summarize_entries_llm()`, `summarize_entries_concat()` |
| `crates/agentos-kernel/src/task_executor.rs` | Pass `agent_id` to `push_entry()` calls; call `increment_references()` after tool results pushed |
| `crates/agentos-kernel/src/context_injector.rs` | Pass `agent_id` to `push_entry()` calls |
| `crates/agentos-kernel/src/kernel.rs` | Update `ContextManager` construction to pass new dependencies |

---

## Testing Strategy

### Feature 1: LLM Summarization
- Unit test: `extract_compressible()` returns correct entries, skips pinned/System
- Unit test: `insert_summary_entry()` places entry after System entries
- Unit test: `summarize_entries_concat()` produces expected format (existing behavior)
- Integration test: `push_entry()` with MockLLM returns LLM-generated summary
- Integration test: `push_entry()` with failing MockLLM falls back to concat
- Integration test: `push_entry()` with `summarization_mode = "concat"` skips LLM

### Feature 2: Context-Loss Notice
- Unit test: `upsert_context_notice()` inserts notice when none exists
- Unit test: `upsert_context_notice()` updates existing notice (no duplicate)
- Unit test: notice is pinned and has correct importance/category
- Integration test: after compression, context contains the notice entry

### Feature 3: Reference Count Tracking
- Unit test: `increment_references_for_tool_call_ids()` increments correct entries
- Unit test: entries without matching IDs are unchanged
- Unit test: SemanticEviction with reference_count > 0 preserves referenced entries over unreferenced ones
- Integration test: full push_entry + tool result cycle produces non-zero reference counts

---

## Non-Goals

- **Configurable eviction weights.** The 0.4/0.3/0.3 split in `evict_by_semantic_score()` is fine. Changing it is a separate concern.
- **Cross-task memory improvements.** Episodic/semantic/procedural memory stores are working correctly. This spec only addresses within-task context management.
- **Streaming summarization.** The summarization call is a simple `infer()`, not streamed. The summary is short enough that streaming adds no value.
- **Per-agent summarization config.** The config is kernel-wide. Per-agent overrides would add complexity for minimal benefit at this stage.
