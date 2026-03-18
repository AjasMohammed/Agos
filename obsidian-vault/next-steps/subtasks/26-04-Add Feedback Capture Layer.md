---
title: Add Feedback Capture Layer
tags:
  - testing
  - llm
  - agent
  - next-steps
date: 2026-03-18
status: complete
effort: 2d
priority: high
---

# Add Feedback Capture Layer

> Enhance feedback collection with cost tracking, per-turn timing, automatic feedback on errors, deduplication, and aggregate statistics.

---

## Why This Subtask

Basic feedback parsing exists from subtask 01. This subtask adds intelligence: auto-generating feedback when tools error, tracking cost per inference call, measuring latency per turn, and deduplicating similar observations across multiple runs.

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Cost tracking | `total_cost_usd: 0.0` | Uses `calculate_inference_cost()` + `default_pricing_table()` from `crates/agentos-llm/src/types.rs` (lines 65-131) |
| Auto-feedback | Only explicit `[FEEDBACK]` blocks | Also auto-generates entries on tool errors, inference errors, and scenario timeouts |
| Per-turn timing | `duration_ms` on ScenarioResult only | `TurnMetrics` struct with inference_ms, tool_execution_ms, tokens, cost |
| Deduplication | None | `FeedbackCollector::deduplicate()` merges >80% word overlap entries in same category |
| Stats | None | `FeedbackCollector::stats()` returns `FeedbackStats` with counts by category/severity/scenario |

## What to Do

1. Create `crates/agentos-agent-tester/src/auto_feedback.rs`:
   - `feedback_from_tool_error(scenario, turn, tool_name, error) -> FeedbackEntry` -- classifies by error type (PermissionDenied -> Security/Warning; ToolNotFound -> Usability/Error; other -> Correctness/Error)
   - `feedback_from_inference_error(scenario, turn, error) -> FeedbackEntry`
   - `feedback_from_timeout(scenario, max_turns) -> FeedbackEntry`

2. Add `TurnMetrics` struct to `crates/agentos-agent-tester/src/scenarios/mod.rs`:
   ```rust
   pub struct TurnMetrics {
       pub turn: usize,
       pub inference_ms: u64,
       pub tool_execution_ms: Option<u64>,
       pub input_tokens: u64,
       pub output_tokens: u64,
       pub cost_usd: f64,
       pub tool_called: Option<String>,
       pub tool_succeeded: bool,
   }
   ```
   Add `pub turn_metrics: Vec<TurnMetrics>` to `ScenarioResult`.

3. In `crates/agentos-agent-tester/src/harness.rs`:
   - Add `pricing: ModelPricing` field to `TestHarness`
   - In `boot()`, call `find_pricing(provider, model)` using `default_pricing_table()` from `agentos_llm`
   - In `run_scenario()`, after each `infer()`:
     - Time the call with `std::time::Instant::now()`
     - Calculate cost: `calculate_inference_cost(&result.tokens_used, &self.pricing)`
     - Build `TurnMetrics` and push to vec
   - In tool execution: time the call, record success/failure
   - On tool error: call `auto_feedback::feedback_from_tool_error()` and add to collector
   - On inference error: call `auto_feedback::feedback_from_inference_error()`
   - On turn exhaustion: call `auto_feedback::feedback_from_timeout()`

4. In `crates/agentos-agent-tester/src/feedback.rs`:
   - Add `FeedbackStats` struct with `total_entries`, `by_category: HashMap<String, usize>`, `by_severity: HashMap<String, usize>`, `by_scenario: HashMap<String, usize>`
   - Add `FeedbackCollector::stats(&self) -> FeedbackStats`
   - Add `FeedbackCollector::deduplicate(&mut self)` using `word_overlap()` helper
   - Add `fn word_overlap(a: &str, b: &str) -> f64` (Jaccard similarity on whitespace-split words)

5. Update `src/lib.rs` to add `pub mod auto_feedback;`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-agent-tester/src/auto_feedback.rs` | New -- 3 auto-feedback generators |
| `crates/agentos-agent-tester/src/feedback.rs` | Add `FeedbackStats`, `deduplicate()`, `stats()`, `word_overlap()` |
| `crates/agentos-agent-tester/src/harness.rs` | Add `pricing` field, cost calc, timing, auto-feedback calls |
| `crates/agentos-agent-tester/src/scenarios/mod.rs` | Add `TurnMetrics`, update `ScenarioResult` |
| `crates/agentos-agent-tester/src/lib.rs` | Add `pub mod auto_feedback;` |

## Prerequisites

[[26-02-Implement LLM Driver Loop]] must be complete first.

## Test Plan

- `cargo build -p agentos-agent-tester` compiles
- `cargo test -p agentos-agent-tester` passes
- Test: `feedback_from_tool_error("test", 1, "file-reader", "PermissionDenied: missing fs.user_data")` produces Security/Warning entry
- Test: `feedback_from_tool_error("test", 1, "fake", "ToolNotFound")` produces Usability/Error entry
- Test: `feedback_from_timeout("test", 10)` produces Usability/Warning entry
- Test: Two feedback entries with identical observations dedup to one with "(observed 2 times)"
- Test: Entries in different categories do not dedup even if text is similar
- Test: `word_overlap("hello world", "hello world")` returns 1.0
- Test: `word_overlap("hello", "goodbye")` returns 0.0
- Test: `stats()` counts correctly with 3 entries across 2 categories
- Test: `find_pricing("anthropic", "claude-sonnet-4-6")` returns non-zero pricing
- Test: `find_pricing("unknown", "unknown")` returns zero pricing

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
```
