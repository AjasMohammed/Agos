---
title: "Phase 6: Cost Attribution and Token Estimation"
tags:
  - llm
  - v3
  - plan
date: 2026-03-24
status: planned
effort: 1d
priority: high
---

# Phase 6: Cost Attribution and Token Estimation

> Attach `InferenceCost` to every `InferenceResult`, add pre-flight token estimation to prevent context overflow, and wire the adapter-computed cost into the kernel's `CostTracker`.

---

## Why This Phase

Cost tracking exists in two disconnected places today:

1. `agentos-llm/src/types.rs` has `calculate_inference_cost()`, `ModelPricing`, and `InferenceCost` -- but nothing calls them.
2. `agentos-kernel/src/cost_tracker.rs` tracks per-agent budgets but recalculates cost independently.

Additionally, there is no pre-flight token estimation. The adapter sends the full context to the provider and discovers overflow only when the API returns a 400 error, wasting time and potentially money.

After this phase:
- Every `InferenceResult` carries its `InferenceCost` computed from the pricing table.
- The kernel `CostTracker` reads the pre-computed cost instead of recalculating.
- Before sending a request, the adapter estimates token count and returns `AgentOSError::ContextOverflow` if the context exceeds the model's window.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `InferenceResult.cost` | Always `None` | Set by adapter after every inference |
| `calculate_inference_cost()` | Dead code in types.rs | Called by each adapter's response parser |
| Token estimation | None | `estimate_tokens(context, tools) -> u64` method on adapters |
| Context overflow | Discovered at API call time (400 error) | Detected pre-flight with advisory error |
| Kernel CostTracker | Recalculates cost from usage + model | Reads `InferenceResult.cost` directly |
| Pricing table | Hardcoded in `default_pricing_table()` | Adapter stores `ModelPricing` at construction; configurable |

---

## What to Do

### Step 1: Add `ModelPricing` to adapter structs

Each adapter struct gets a `pricing: ModelPricing` field set at construction time from `default_pricing_table()`:

```rust
impl OpenAICore {
    pub fn new(api_key: SecretString, model: String) -> Self {
        let pricing = default_pricing_table()
            .into_iter()
            .find(|p| p.provider == "openai" && (p.model == model || p.model == "*"))
            .unwrap_or(ModelPricing {
                provider: "openai".into(),
                model: model.clone(),
                input_per_1k: 0.0,
                output_per_1k: 0.0,
            });
        Self {
            // ... existing fields ...
            pricing,
        }
    }
}
```

### Step 2: Compute cost after every inference

In each adapter's response parsing, after extracting `TokenUsage`:

```rust
let cost = calculate_inference_cost(&tokens_used, &self.pricing);
// Then in InferenceResult:
InferenceResult {
    // ... existing fields ...
    cost: Some(cost),
}
```

### Step 3: Add `estimate_tokens` method to `LLMCore` trait

Open `crates/agentos-llm/src/traits.rs`:

```rust
/// Estimate the token count for a context window + tools.
/// Used for pre-flight overflow detection. The default implementation
/// uses a simple characters/4 heuristic.
fn estimate_tokens(&self, context: &ContextWindow, tools: &[ToolManifest]) -> u64 {
    let chars: usize = context.active_entries().iter()
        .map(|e| e.content.len())
        .sum::<usize>()
        + tools.iter()
            .map(|t| t.manifest.description.len() + t.manifest.name.len() + 100) // overhead per tool
            .sum::<usize>();
    (chars as f64 / 4.0).ceil() as u64
}
```

### Step 4: Add pre-flight check in `infer_with_tools`

In each adapter's `infer_with_tools`, before sending the request:

```rust
let estimated = self.estimate_tokens(context, tools);
let max = self.capabilities.context_window_tokens;
if estimated > max {
    return Err(AgentOSError::LLMError {
        provider: self.provider_name().to_string(),
        reason: format!(
            "Estimated token count ({}) exceeds model context window ({}). \
             Reduce context or use a model with a larger window.",
            estimated, max
        ),
    });
}
```

This is advisory -- the estimate may be inaccurate, but it catches obvious overflow before wasting an API call.

### Step 5: Update kernel CostTracker to use adapter-provided cost

Open `crates/agentos-kernel/src/cost_tracker.rs`. Find the place where cost is recorded after inference and use `result.cost` if present instead of recalculating:

```rust
if let Some(cost) = &inference_result.cost {
    self.cost_tracker.record_inference_cost(agent_id, cost).await;
} else {
    // Fallback: calculate from usage (for adapters that haven't been updated)
    // ... existing calculation ...
}
```

### Step 6: Add `with_pricing` builder method to adapters

Allow overriding the default pricing:

```rust
pub fn with_pricing(mut self, pricing: ModelPricing) -> Self {
    self.pricing = pricing;
    self
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/traits.rs` | Add `estimate_tokens` method with default implementation |
| `crates/agentos-llm/src/openai.rs` | Add `pricing` field, compute cost, add pre-flight check |
| `crates/agentos-llm/src/anthropic.rs` | Add `pricing` field, compute cost, add pre-flight check |
| `crates/agentos-llm/src/gemini.rs` | Add `pricing` field, compute cost, add pre-flight check |
| `crates/agentos-llm/src/ollama.rs` | Add `pricing` field (zero cost), compute cost |
| `crates/agentos-llm/src/custom.rs` | Add `pricing` field, compute cost |
| `crates/agentos-kernel/src/cost_tracker.rs` | Use `InferenceResult.cost` when available |

---

## Prerequisites

[[01-core-types-and-trait-redesign]] must be complete (`InferenceResult.cost` field exists).

---

## Test Plan

- `cargo build --workspace` must pass
- Add test `test_cost_attached_to_inference_result` -- mock a response with known token counts, verify `InferenceResult.cost` has correct USD values
- Add test `test_estimate_tokens_heuristic` -- context with known character count, verify estimate is approximately chars/4
- Add test `test_preflight_overflow_detection` -- context that exceeds `context_window_tokens`, verify error is returned before HTTP call
- Add test `test_pricing_lookup_fallback` -- model not in pricing table, verify zero-cost fallback
- Existing cost tracker tests must pass

---

## Verification

```bash
cargo build --workspace
cargo test --workspace -- --nocapture
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
