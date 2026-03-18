---
title: "Phase 04: Structured Feedback Capture"
tags:
  - testing
  - llm
  - agent
  - plan
date: 2026-03-18
status: planned
effort: 2d
priority: high
---

# Phase 04: Structured Feedback Capture

> Enhance the feedback collection system with cost tracking, token usage analysis, response time metrics, automatic feedback extraction from error conditions, and a deduplication/aggregation layer.

---

## Why This Phase

Phase 01 created basic `FeedbackEntry` and `parse_feedback()`. This phase adds the intelligence layer: automatic feedback when tools error, cost attribution per scenario, timing metrics, and deduplication so the final report is not flooded with repeated observations from multiple runs.

---

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `FeedbackCollector` | Simple `Vec<FeedbackEntry>` append | Aggregation, deduplication, auto-feedback on errors, category stats |
| Cost tracking | `total_cost_usd: 0.0` placeholder | Uses `calculate_inference_cost()` from `agentos-llm/src/types.rs` with the default pricing table |
| Error feedback | Only explicit `[FEEDBACK]` blocks from LLM | Auto-generates feedback entries when tool calls fail, permissions denied, or scenarios error |
| Timing | `duration_ms` on `ScenarioResult` only | Per-turn timing, per-tool-call timing, LLM inference latency tracking |
| Deduplication | None | Observations with >80% string similarity within the same category are merged |

---

## What to Do

### 1. Add cost tracking to `TestHarness::run_scenario()`

Open `crates/agentos-agent-tester/src/harness.rs` and update the driver loop to calculate cost after each inference call:

```rust
use agentos_llm::{calculate_inference_cost, default_pricing_table, ModelPricing};

// At harness construction, resolve pricing for the selected model
fn find_pricing(provider: &str, model: &str) -> ModelPricing {
    let table = default_pricing_table();
    table.iter()
        .find(|p| p.provider == provider && (p.model == model || p.model == "*"))
        .cloned()
        .unwrap_or(ModelPricing {
            provider: provider.to_string(),
            model: model.to_string(),
            input_per_1k: 0.0,
            output_per_1k: 0.0,
        })
}

// In run_scenario(), after each infer() call:
let cost = calculate_inference_cost(&infer_result.tokens_used, &self.pricing);
total_cost_usd += cost.total_cost_usd;
```

Add a `pricing: ModelPricing` field to `TestHarness` and initialize it in `boot()`.

### 2. Add per-turn timing to `ScenarioResult`

Add a new type for turn-level metrics:

```rust
// In src/scenarios/mod.rs or a new src/metrics.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
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

Add `pub turn_metrics: Vec<TurnMetrics>` to `ScenarioResult`. Populate it in the driver loop by timing each `infer()` call and each `execute_tool_call()`.

### 3. Add automatic feedback generation on errors

Create `src/auto_feedback.rs`:

```rust
use crate::feedback::{FeedbackCategory, FeedbackEntry, FeedbackSeverity};

/// Generate automatic feedback when a tool call fails.
pub fn feedback_from_tool_error(
    scenario: &str,
    turn: usize,
    tool_name: &str,
    error: &str,
) -> FeedbackEntry {
    let severity = if error.contains("PermissionDenied") {
        FeedbackSeverity::Warning
    } else {
        FeedbackSeverity::Error
    };

    let category = if error.contains("PermissionDenied") {
        FeedbackCategory::Security
    } else if error.contains("not found") || error.contains("ToolNotFound") {
        FeedbackCategory::Usability
    } else {
        FeedbackCategory::Correctness
    };

    FeedbackEntry {
        scenario: scenario.to_string(),
        turn,
        category,
        severity,
        observation: format!("Tool '{}' failed with error: {}", tool_name, error),
        suggestion: Some(classify_error_suggestion(error)),
        context: Some(format!("Attempted to call tool '{}'", tool_name)),
    }
}

/// Generate feedback when LLM inference itself fails.
pub fn feedback_from_inference_error(
    scenario: &str,
    turn: usize,
    error: &str,
) -> FeedbackEntry {
    FeedbackEntry {
        scenario: scenario.to_string(),
        turn,
        category: FeedbackCategory::Correctness,
        severity: FeedbackSeverity::Error,
        observation: format!("LLM inference failed: {}", error),
        suggestion: Some("Check LLM connectivity and API key validity".to_string()),
        context: Some("During scenario execution".to_string()),
    }
}

/// Generate feedback when scenario times out (exhausts turns).
pub fn feedback_from_timeout(
    scenario: &str,
    max_turns: usize,
) -> FeedbackEntry {
    FeedbackEntry {
        scenario: scenario.to_string(),
        turn: max_turns,
        category: FeedbackCategory::Usability,
        severity: FeedbackSeverity::Warning,
        observation: format!(
            "Scenario did not complete within {} turns. The LLM could not achieve the goal.",
            max_turns
        ),
        suggestion: Some("Consider whether the scenario goal is achievable with the available tools, or improve tool/system prompt clarity".to_string()),
        context: Some("Scenario turn budget exhausted".to_string()),
    }
}

fn classify_error_suggestion(error: &str) -> String {
    if error.contains("PermissionDenied") {
        "Error message should tell the agent which specific permission is needed and how to request it".to_string()
    } else if error.contains("path traversal") || error.contains("..") {
        "Path traversal denial is correctly enforced. Error message quality is good.".to_string()
    } else if error.contains("not found") {
        "Consider suggesting similar tool names or listing available tools in the error".to_string()
    } else {
        "Review error message for agent-friendliness: is it actionable?".to_string()
    }
}
```

### 4. Wire auto-feedback into the driver loop

In `TestHarness::run_scenario()`, update the tool execution branch:

```rust
// In the tool call execution path:
let tool_start = std::time::Instant::now();
let tool_result_str = self.execute_tool_call(&tool_call).await;
let tool_ms = tool_start.elapsed().as_millis() as u64;

let tool_succeeded = !tool_result_str.starts_with("Tool execution failed:");

if !tool_succeeded {
    // Auto-generate feedback for the error
    let auto_fb = auto_feedback::feedback_from_tool_error(
        &scenario.name, turn, &tool_call.name, &tool_result_str,
    );
    collector.add(auto_fb);
}
```

Also add auto-feedback when inference fails and when the scenario times out.

### 5. Add deduplication to `FeedbackCollector`

Enhance `FeedbackCollector` with a dedup method:

```rust
impl FeedbackCollector {
    /// Deduplicate feedback entries with similar observations.
    /// Entries within the same category that share >80% word overlap are merged,
    /// incrementing a count field.
    pub fn deduplicate(&mut self) {
        let mut deduped: Vec<(FeedbackEntry, usize)> = Vec::new();

        for entry in self.entries.drain(..) {
            let mut merged = false;
            for (existing, count) in &mut deduped {
                if existing.category == entry.category
                    && word_overlap(&existing.observation, &entry.observation) > 0.8
                {
                    *count += 1;
                    merged = true;
                    break;
                }
            }
            if !merged {
                deduped.push((entry, 1));
            }
        }

        self.entries = deduped.into_iter().map(|(mut e, count)| {
            if count > 1 {
                e.observation = format!("{} (observed {} times)", e.observation, count);
            }
            e
        }).collect();
    }
}

fn word_overlap(a: &str, b: &str) -> f64 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }
    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();
    intersection as f64 / union as f64
}
```

### 6. Add aggregate statistics to `FeedbackCollector`

```rust
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackStats {
    pub total_entries: usize,
    pub by_category: HashMap<String, usize>,
    pub by_severity: HashMap<String, usize>,
    pub by_scenario: HashMap<String, usize>,
}

impl FeedbackCollector {
    pub fn stats(&self) -> FeedbackStats {
        let mut by_category = HashMap::new();
        let mut by_severity = HashMap::new();
        let mut by_scenario = HashMap::new();

        for entry in &self.entries {
            *by_category.entry(format!("{:?}", entry.category)).or_insert(0) += 1;
            *by_severity.entry(format!("{:?}", entry.severity)).or_insert(0) += 1;
            *by_scenario.entry(entry.scenario.clone()).or_insert(0) += 1;
        }

        FeedbackStats {
            total_entries: self.entries.len(),
            by_category,
            by_severity,
            by_scenario,
        }
    }
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-agent-tester/src/auto_feedback.rs` | New file -- automatic feedback generators |
| `crates/agentos-agent-tester/src/feedback.rs` | Add `FeedbackStats`, `deduplicate()`, `stats()`, `word_overlap()` |
| `crates/agentos-agent-tester/src/harness.rs` | Add `pricing` field, cost calculation, per-turn timing, auto-feedback wiring |
| `crates/agentos-agent-tester/src/scenarios/mod.rs` | Add `TurnMetrics` type, update `ScenarioResult` |
| `crates/agentos-agent-tester/src/lib.rs` | Add `pub mod auto_feedback;` |

---

## Dependencies

[[02-llm-driver-loop]] must be complete first -- the driver loop is where cost tracking and auto-feedback are wired in.

---

## Test Plan

- `cargo build -p agentos-agent-tester` must compile.
- `cargo test -p agentos-agent-tester` must pass.
- Add unit tests for auto-feedback:
  - Test: `feedback_from_tool_error("test", 1, "file-reader", "PermissionDenied { ... }")` produces a `Security` category, `Warning` severity entry.
  - Test: `feedback_from_tool_error("test", 1, "nonexistent", "ToolNotFound")` produces a `Usability` category entry.
  - Test: `feedback_from_timeout("test", 10)` produces a `Usability` category, `Warning` severity entry.
- Add unit tests for deduplication:
  - Test: Two entries with identical observations in the same category are merged into one with "(observed 2 times)".
  - Test: Two entries with different categories are NOT merged even if observations are similar.
  - Test: Two entries with <80% word overlap are NOT merged.
- Add unit tests for stats:
  - Test: `stats()` correctly counts entries by category and severity.
- Add unit test for cost calculation:
  - Test: `find_pricing("anthropic", "claude-sonnet-4-6")` returns the correct pricing from the default table.
  - Test: `find_pricing("unknown", "unknown")` returns zero pricing.
- Add unit test for `word_overlap()`:
  - Test: Identical strings return 1.0.
  - Test: Completely different strings return 0.0.
  - Test: Partial overlap returns value between 0 and 1.

---

## Verification

```bash
cargo build -p agentos-agent-tester
cargo test -p agentos-agent-tester -- --nocapture
```
