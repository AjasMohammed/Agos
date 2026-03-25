---
title: "Phase 1: Core Types and Trait Redesign"
tags:
  - llm
  - v3
  - plan
date: 2026-03-24
status: complete
effort: 2d
priority: critical
---

# Phase 1: Core Types and Trait Redesign

> Add `StopReason`, `InferenceOptions`, enhanced `InferenceEvent`, and expanded `ModelCapabilities` to the `agentos-llm` crate. Extend the `LLMCore` trait with options support. All changes are additive -- no existing code breaks.

---

## Why This Phase

Every subsequent phase depends on the core type definitions. `StopReason` drives the kernel loop (Phase 3), `InferenceEvent` variants enable streaming (Phase 4), `InferenceOptions` enables tool choice control (Phase 8), and `ModelCapabilities` extensions let the kernel query what the adapter supports. This must land first.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Stop reason | Not tracked | `StopReason` enum on `InferenceResult` |
| Inference options | Hardcoded per-adapter | `InferenceOptions` struct passed to trait methods |
| `InferenceEvent` | `Token`, `Done`, `Error` | Add `ToolCallStart`, `ToolCallDelta`, `ToolCallComplete`, `Usage` |
| `ModelCapabilities` | 5 fields | Add `supports_streaming`, `supports_parallel_tools`, `supports_prompt_caching`, `supports_thinking` |
| `InferenceResult` | No cost, no stop_reason | Add `stop_reason: StopReason`, `cost: Option<InferenceCost>`, `cached_tokens: u64` |
| `InferenceToolCall` | 4 fields | Add `status: ToolCallStatus` for streaming accumulation |
| `LLMCore` trait | `infer`, `infer_with_tools` | Add `infer_with_options` with default that delegates to `infer_with_tools` |

---

## What to Do

### Step 1: Add `StopReason` enum to `types.rs`

Open `crates/agentos-llm/src/types.rs`. Add:

```rust
/// Why the model stopped generating.
/// Used by the kernel to decide whether to continue the agentic loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// Model finished naturally (OpenAI: "stop", Anthropic: "end_turn", Gemini: "STOP").
    EndTurn,
    /// Model wants to call one or more tools (OpenAI: "tool_calls", Anthropic: "tool_use", Gemini: "FUNCTION_CALL").
    ToolUse,
    /// Model hit the max_tokens limit and was truncated.
    MaxTokens,
    /// Content was filtered by the provider's safety system.
    ContentFilter,
    /// A stop sequence was matched.
    StopSequence,
    /// Unknown or provider-specific reason.
    Other(String),
}

impl Default for StopReason {
    fn default() -> Self {
        StopReason::EndTurn
    }
}
```

### Step 2: Add `InferenceOptions` struct to `types.rs`

```rust
/// Per-inference configuration options.
/// Passed to `infer_with_options` to control behavior for a single call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceOptions {
    /// Tool choice strategy. None = provider default (usually "auto").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether to request streaming. None = non-streaming.
    #[serde(default)]
    pub stream: bool,
    /// Temperature override for this call. None = model default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Max output tokens override. None = adapter default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Request structured JSON output.
    #[serde(default)]
    pub json_mode: bool,
    /// Seed for reproducible output (OpenAI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

/// Tool choice strategy for inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolChoice {
    /// Let the model decide (default).
    Auto,
    /// Model must not call any tools.
    None,
    /// Model must call at least one tool.
    Required,
    /// Model must call this specific tool.
    Specific(String),
}
```

### Step 3: Extend `InferenceResult`

Add fields to the existing struct (with `#[serde(default)]` for backward compat):

```rust
pub struct InferenceResult {
    pub text: String,
    pub tokens_used: TokenUsage,
    pub model: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub tool_calls: Vec<InferenceToolCall>,
    #[serde(default)]
    pub uncertainty: Option<UncertaintyDeclaration>,
    /// Why the model stopped generating. Drives kernel agentic loop control.
    #[serde(default)]
    pub stop_reason: StopReason,
    /// Cost of this inference call, if computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<InferenceCost>,
    /// Number of prompt tokens that were cache hits (Anthropic/OpenAI).
    #[serde(default)]
    pub cached_tokens: u64,
}
```

### Step 4: Extend `InferenceEvent`

Add new variants without removing existing ones:

```rust
pub enum InferenceEvent {
    /// A chunk of generated text.
    Token(String),
    /// A tool call has started (name known, arguments streaming).
    ToolCallStart {
        index: usize,
        id: Option<String>,
        tool_name: String,
    },
    /// A chunk of tool call arguments (for streaming accumulation).
    ToolCallDelta {
        index: usize,
        arguments_chunk: String,
    },
    /// A tool call is fully assembled and ready for execution.
    ToolCallComplete(InferenceToolCall),
    /// Token usage update (may arrive mid-stream or at end).
    Usage(TokenUsage),
    /// The final result.
    Done(InferenceResult),
    /// An error occurred.
    Error(String),
}
```

### Step 5: Extend `ModelCapabilities`

```rust
pub struct ModelCapabilities {
    pub context_window_tokens: u64,
    pub supports_images: bool,
    pub supports_tool_calling: bool,
    pub supports_json_mode: bool,
    #[serde(default)]
    pub max_output_tokens: u64,
    /// Whether the adapter implements real streaming (not fake fallback).
    #[serde(default)]
    pub supports_streaming: bool,
    /// Whether the model can emit multiple tool calls in one turn.
    #[serde(default)]
    pub supports_parallel_tools: bool,
    /// Whether prompt caching is available (reduces cost on repeated context).
    #[serde(default)]
    pub supports_prompt_caching: bool,
    /// Whether the model supports extended thinking / chain-of-thought.
    #[serde(default)]
    pub supports_thinking: bool,
    /// Whether structured JSON output mode is enforced (not just JSON in instructions).
    #[serde(default)]
    pub supports_structured_output: bool,
}
```

### Step 6: Add `infer_with_options` to `LLMCore` trait

Open `crates/agentos-llm/src/traits.rs`. Add a new method with a default implementation that delegates to `infer_with_tools`:

```rust
/// Inference with full options control. This is the primary method for
/// agentic workflows. The default implementation ignores options and
/// delegates to `infer_with_tools()`.
async fn infer_with_options(
    &self,
    context: &ContextWindow,
    tools: &[ToolManifest],
    options: &InferenceOptions,
) -> Result<InferenceResult, AgentOSError> {
    let _ = options;
    self.infer_with_tools(context, tools).await
}
```

### Step 7: Re-export new types in `lib.rs`

Add to the `pub use types::` block:

```rust
pub use types::{
    // ... existing exports ...
    InferenceOptions, StopReason, ToolChoice,
};
```

### Step 8: Update all adapters to set `stop_reason: StopReason::EndTurn` and `cost: None` and `cached_tokens: 0`

In each adapter's `InferenceResult` construction, add the new fields with defaults. This is mechanical -- just add `stop_reason: StopReason::EndTurn, cost: None, cached_tokens: 0` to every `InferenceResult { ... }` literal.

Files: `openai.rs`, `anthropic.rs`, `gemini.rs`, `ollama.rs`, `custom.rs`, `mock.rs`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/types.rs` | Add `StopReason`, `InferenceOptions`, `ToolChoice`; extend `InferenceResult`, `InferenceEvent`, `ModelCapabilities` |
| `crates/agentos-llm/src/traits.rs` | Add `infer_with_options` method with default |
| `crates/agentos-llm/src/lib.rs` | Re-export new types |
| `crates/agentos-llm/src/openai.rs` | Add default new fields to `InferenceResult` literals |
| `crates/agentos-llm/src/anthropic.rs` | Add default new fields to `InferenceResult` literals |
| `crates/agentos-llm/src/gemini.rs` | Add default new fields to `InferenceResult` literals |
| `crates/agentos-llm/src/ollama.rs` | Add default new fields to `InferenceResult` literals |
| `crates/agentos-llm/src/custom.rs` | Add default new fields to `InferenceResult` literals |
| `crates/agentos-llm/src/mock.rs` | Add default new fields to `InferenceResult` literals |

---

## Prerequisites

None -- this is the first phase.

---

## Test Plan

- `cargo build -p agentos-llm` must compile with zero errors
- `cargo test -p agentos-llm` -- all existing tests pass (they construct `InferenceResult` which now has new `#[serde(default)]` fields)
- Add test `test_stop_reason_default` asserting `StopReason::default() == StopReason::EndTurn`
- Add test `test_inference_options_default` asserting `InferenceOptions::default()` has `stream: false`, `tool_choice: None`, `json_mode: false`
- Add test `test_inference_result_serde_roundtrip` asserting the new fields survive JSON serialization/deserialization
- `cargo build --workspace` must pass (no downstream breakage)
- `cargo test --workspace` must pass

---

## Verification

```bash
cargo build -p agentos-llm
cargo test -p agentos-llm -- --nocapture
cargo build --workspace
cargo test --workspace
cargo clippy -p agentos-llm -- -D warnings
cargo fmt --all -- --check
```
