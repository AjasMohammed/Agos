---
title: "Phase 7: Enhanced Mock and Testing Infrastructure"
tags:
  - llm
  - v3
  - plan
date: 2026-03-24
status: complete
effort: 1d
priority: high
---

# Phase 7: Enhanced Mock and Testing Infrastructure

> Upgrade `MockLLMCore` to support tool calls, stop reasons, streaming simulation, and detailed call history -- enabling comprehensive kernel integration testing without real LLM APIs.

---

## Why This Phase

The current `MockLLMCore` returns canned strings from a `Vec<String>`. It cannot:

- Return `InferenceResult` values with tool calls or specific stop reasons
- Simulate streaming events
- Record what context and tools were passed to it (for assertion in tests)
- Return different responses based on the context content

Kernel integration tests that exercise the agentic tool loop, stop reason handling, and streaming all require a mock that speaks the full `LLMCore` interface. Without this, the Phase 3 kernel migration cannot be properly tested.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Response type | `Vec<String>` (text only) | `Vec<MockResponse>` with text, tool_calls, stop_reason |
| Tool call simulation | Not possible | `MockResponse::with_tool_calls(vec![...])` |
| Stop reason | Always `StopReason::EndTurn` (default) | Configurable per response |
| Call history | `call_count: AtomicUsize` | `Vec<MockCallRecord>` with context snapshot and tools |
| Streaming | Uses trait default (fake) | Overrides `infer_stream_with_tools` to emit events from `MockResponse` |
| Capabilities | Fixed (no tools, no streaming) | Configurable |

---

## What to Do

### Step 1: Define `MockResponse` and `MockCallRecord`

Open `crates/agentos-llm/src/mock.rs`. Replace the simple string-based mock:

```rust
use crate::types::*;
use agentos_types::*;
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

/// A preconfigured response for the mock adapter.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub text: String,
    pub tool_calls: Vec<InferenceToolCall>,
    pub stop_reason: StopReason,
    pub tokens_used: TokenUsage,
}

impl MockResponse {
    /// Create a simple text-only response.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            tokens_used: TokenUsage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 },
        }
    }

    /// Create a response with tool calls.
    pub fn with_tool_calls(mut self, calls: Vec<InferenceToolCall>) -> Self {
        self.stop_reason = StopReason::ToolUse;
        self.tool_calls = calls;
        self
    }

    /// Override the stop reason.
    pub fn with_stop_reason(mut self, reason: StopReason) -> Self {
        self.stop_reason = reason;
        self
    }

    /// Override token usage.
    pub fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.tokens_used = usage;
        self
    }
}

/// Record of a call made to the mock adapter.
#[derive(Debug, Clone)]
pub struct MockCallRecord {
    /// The context window entries (role + content) at call time.
    pub context_entries: Vec<(String, String)>,
    /// Tool manifest names passed.
    pub tool_names: Vec<String>,
    /// Which method was called.
    pub method: MockCallMethod,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MockCallMethod {
    Infer,
    InferWithTools,
    InferStream,
    InferStreamWithTools,
}
```

### Step 2: Rewrite `MockLLMCore`

```rust
pub struct MockLLMCore {
    responses: Mutex<Vec<MockResponse>>,
    call_count: AtomicUsize,
    call_history: Mutex<Vec<MockCallRecord>>,
    capabilities: ModelCapabilities,
}

impl MockLLMCore {
    /// Create with typed responses.
    pub fn new(responses: Vec<MockResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            call_count: AtomicUsize::new(0),
            call_history: Mutex::new(Vec::new()),
            capabilities: ModelCapabilities {
                context_window_tokens: 8192,
                supports_images: false,
                supports_tool_calling: true,  // Changed: mock supports tools
                supports_json_mode: false,
                max_output_tokens: 4096,
                supports_streaming: true,     // Changed: mock supports streaming
                supports_parallel_tools: true,
                supports_prompt_caching: false,
                supports_thinking: false,
                supports_structured_output: false,
            },
        }
    }

    /// Convenience: create from plain strings (backward compatible).
    pub fn from_strings(texts: Vec<String>) -> Self {
        Self::new(texts.into_iter().map(MockResponse::text).collect())
    }

    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    /// Get a clone of all call records for test assertions.
    pub fn call_history(&self) -> Vec<MockCallRecord> {
        self.call_history.lock().unwrap().clone()
    }

    /// Get the Nth call record.
    pub fn call_at(&self, index: usize) -> Option<MockCallRecord> {
        self.call_history.lock().unwrap().get(index).cloned()
    }

    fn pop_response(&self) -> MockResponse {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            MockResponse::text("No more mock responses")
        } else {
            responses.remove(0)
        }
    }

    fn record_call(&self, context: &ContextWindow, tools: &[ToolManifest], method: MockCallMethod) {
        let entries = context.active_entries().iter()
            .map(|e| (format!("{:?}", e.role), e.content.clone()))
            .collect();
        let tool_names = tools.iter().map(|t| t.manifest.name.clone()).collect();
        self.call_history.lock().unwrap().push(MockCallRecord {
            context_entries: entries,
            tool_names,
            method,
        });
    }
}
```

### Step 3: Implement `LLMCore` for the new mock

```rust
#[async_trait]
impl LLMCore for MockLLMCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        self.infer_with_tools(context, &[]).await
    }

    async fn infer_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
    ) -> Result<InferenceResult, AgentOSError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.record_call(context, tools, MockCallMethod::InferWithTools);
        let resp = self.pop_response();
        Ok(InferenceResult {
            text: resp.text,
            tokens_used: resp.tokens_used,
            model: "mock-model".to_string(),
            duration_ms: 1,
            tool_calls: resp.tool_calls,
            uncertainty: None,
            stop_reason: resp.stop_reason,
            cost: None,
            cached_tokens: 0,
        })
    }

    async fn infer_stream_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.record_call(context, tools, MockCallMethod::InferStreamWithTools);
        let resp = self.pop_response();

        // Simulate streaming: emit text in chunks.
        for chunk in resp.text.as_bytes().chunks(20) {
            let s = String::from_utf8_lossy(chunk).to_string();
            let _ = tx.send(InferenceEvent::Token(s)).await;
        }

        // Emit tool calls.
        for (i, call) in resp.tool_calls.iter().enumerate() {
            let _ = tx.send(InferenceEvent::ToolCallStart {
                index: i,
                id: call.id.clone(),
                tool_name: call.tool_name.clone(),
            }).await;
            let _ = tx.send(InferenceEvent::ToolCallComplete(call.clone())).await;
        }

        let result = InferenceResult {
            text: resp.text,
            tokens_used: resp.tokens_used,
            model: "mock-model".to_string(),
            duration_ms: 1,
            tool_calls: resp.tool_calls,
            uncertainty: None,
            stop_reason: resp.stop_reason,
            cost: None,
            cached_tokens: 0,
        };
        let _ = tx.send(InferenceEvent::Done(result)).await;
        Ok(())
    }

    // ... capabilities, health_check, provider_name, model_name unchanged ...
}
```

### Step 4: Keep backward compatibility

The existing constructor `MockLLMCore::new(Vec<String>)` should still work. Rename the new constructor and provide a compat wrapper:

```rust
impl MockLLMCore {
    /// Legacy constructor for backward compatibility.
    pub fn new(responses: Vec<String>) -> Self {
        Self::from_strings(responses)
    }

    /// Create with typed mock responses.
    pub fn with_responses(responses: Vec<MockResponse>) -> Self {
        // ... the full constructor ...
    }
}
```

### Step 5: Update existing tests

All existing tests that construct `MockLLMCore::new(vec!["...".to_string()])` will continue to work via the backward-compatible constructor.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/mock.rs` | Rewrite with `MockResponse`, `MockCallRecord`, call history, streaming simulation |
| `crates/agentos-llm/src/lib.rs` | Re-export `MockResponse`, `MockCallRecord` |

---

## Prerequisites

- [[01-core-types-and-trait-redesign]] (new `InferenceEvent` variants, `StopReason`)
- [[02-native-tool-result-formatting]] (tool call types)
- [[03-stop-reason-and-kernel-migration]] (kernel uses `StopReason`, mock must produce correct values)

---

## Test Plan

- `cargo test -p agentos-llm` -- all existing mock tests pass with the backward-compatible constructor
- Add test `test_mock_returns_tool_calls` -- mock with `MockResponse::text("thinking").with_tool_calls(...)` returns correct tool_calls and `StopReason::ToolUse`
- Add test `test_mock_call_history` -- after two calls, `call_history()` has two records with correct context entries and tool names
- Add test `test_mock_streaming_emits_events` -- `infer_stream_with_tools` emits Token, ToolCallStart, ToolCallComplete, Done in order
- `cargo test --workspace` -- all kernel integration tests that use MockLLMCore still pass

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
