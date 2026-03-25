---
title: "Phase 8: Provider Failover and Advanced Options"
tags:
  - llm
  - v3
  - plan
date: 2026-03-24
status: complete
effort: 1.5d
priority: medium
---

# Phase 8: Provider Failover and Advanced Options

> Add a `FallbackAdapter` that wraps multiple `LLMCore` instances and fails over to the next when the primary is unhealthy. Implement `InferenceOptions` support in all adapters for tool choice, JSON mode, and temperature control.

---

## Why This Phase

When a provider goes down, the agent is completely stuck. There is no mechanism to try a secondary provider. For production agentic workflows, this is unacceptable -- a 5-minute OpenAI outage should not halt all agents.

Additionally, `InferenceOptions` was added in Phase 1 but no adapter uses it yet. The kernel and chat system need the ability to:
- Force a specific tool call (for guided workflows)
- Disable tool calling (for final answer generation)
- Request JSON mode (for structured output parsing)
- Set temperature per call (lower for tool calls, higher for creative tasks)

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Failover | None | `FallbackAdapter` wrapping `Vec<Arc<dyn LLMCore>>` |
| Tool choice | Hardcoded `"auto"` everywhere | `InferenceOptions.tool_choice` mapped to each provider's format |
| JSON mode | Not used | `InferenceOptions.json_mode` sets `response_format` (OpenAI) or tool-use forcing (Anthropic) |
| Temperature | Not configurable per call | `InferenceOptions.temperature` passed to provider |
| Max tokens override | Only Anthropic has builder | `InferenceOptions.max_tokens` passed to provider |

---

## What to Do

### Step 1: Create `FallbackAdapter` in `crates/agentos-llm/src/fallback.rs`

```rust
use crate::traits::LLMCore;
use crate::types::*;
use agentos_types::*;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

/// An adapter that tries providers in order, failing over on errors.
pub struct FallbackAdapter {
    /// Providers in priority order. First healthy provider is used.
    providers: Vec<Arc<dyn LLMCore>>,
    /// Capabilities of the primary (first) provider.
    capabilities: ModelCapabilities,
}

impl FallbackAdapter {
    pub fn new(providers: Vec<Arc<dyn LLMCore>>) -> Self {
        assert!(!providers.is_empty(), "FallbackAdapter requires at least one provider");
        let capabilities = providers[0].capabilities().clone();
        Self { providers, capabilities }
    }

    async fn find_healthy(&self) -> Option<&Arc<dyn LLMCore>> {
        for provider in &self.providers {
            if provider.health_check().await.is_healthy() {
                return Some(provider);
            }
        }
        None
    }
}

#[async_trait]
impl LLMCore for FallbackAdapter {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        self.infer_with_tools(context, &[]).await
    }

    async fn infer_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
    ) -> Result<InferenceResult, AgentOSError> {
        let mut last_error = None;
        for (i, provider) in self.providers.iter().enumerate() {
            match provider.infer_with_tools(context, tools).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    warn!(
                        provider = provider.provider_name(),
                        index = i,
                        error = %e,
                        "Provider failed, trying next"
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| AgentOSError::LLMError {
            provider: "fallback".to_string(),
            reason: "All providers failed".to_string(),
        }))
    }

    async fn infer_stream_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        let mut last_error = None;
        for (i, provider) in self.providers.iter().enumerate() {
            match provider.infer_stream_with_tools(context, tools, tx.clone()).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(
                        provider = provider.provider_name(),
                        index = i,
                        error = %e,
                        "Streaming provider failed, trying next"
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| AgentOSError::LLMError {
            provider: "fallback".to_string(),
            reason: "All providers failed".to_string(),
        }))
    }

    fn capabilities(&self) -> &ModelCapabilities { &self.capabilities }
    async fn health_check(&self) -> HealthStatus {
        if self.find_healthy().await.is_some() {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy { reason: "All providers unhealthy".into() }
        }
    }
    fn provider_name(&self) -> &str { "fallback" }
    fn model_name(&self) -> &str { self.providers[0].model_name() }
}
```

### Step 2: Implement `InferenceOptions` in OpenAI adapter

Override `infer_with_options` to use the options:

```rust
async fn infer_with_options(
    &self,
    context: &ContextWindow,
    tools: &[ToolManifest],
    options: &InferenceOptions,
) -> Result<InferenceResult, AgentOSError> {
    // ... build request body ...

    // Apply tool_choice from options.
    match &options.tool_choice {
        Some(ToolChoice::Auto) => { body["tool_choice"] = json!("auto"); }
        Some(ToolChoice::None) => { body["tool_choice"] = json!("none"); }
        Some(ToolChoice::Required) => { body["tool_choice"] = json!("required"); }
        Some(ToolChoice::Specific(name)) => {
            body["tool_choice"] = json!({"type": "function", "function": {"name": name}});
        }
        None => { /* leave default "auto" */ }
    }

    if options.json_mode {
        body["response_format"] = json!({"type": "json_object"});
    }
    if let Some(temp) = options.temperature {
        body["temperature"] = json!(temp);
    }
    if let Some(max) = options.max_tokens {
        body["max_tokens"] = json!(max);
    }
    if let Some(seed) = options.seed {
        body["seed"] = json!(seed);
    }

    // ... send request ...
}
```

### Step 3: Implement `InferenceOptions` in Anthropic adapter

```rust
// tool_choice mapping:
match &options.tool_choice {
    Some(ToolChoice::Auto) => { body["tool_choice"] = json!({"type": "auto"}); }
    Some(ToolChoice::None) => { /* omit tools entirely */ }
    Some(ToolChoice::Required) => { body["tool_choice"] = json!({"type": "any"}); }
    Some(ToolChoice::Specific(name)) => {
        body["tool_choice"] = json!({"type": "tool", "name": name});
    }
    None => {}
}
if let Some(temp) = options.temperature {
    body["temperature"] = json!(temp);
}
if let Some(max) = options.max_tokens {
    body["max_tokens"] = json!(max);
}
```

### Step 4: Implement `InferenceOptions` in Gemini adapter

```rust
// tool_choice mapping via functionCallingConfig:
if let Some(choice) = &options.tool_choice {
    let mode = match choice {
        ToolChoice::Auto => "AUTO",
        ToolChoice::None => "NONE",
        ToolChoice::Required | ToolChoice::Specific(_) => "ANY",
    };
    body["toolConfig"] = json!({"functionCallingConfig": {"mode": mode}});
}
if let Some(temp) = options.temperature {
    body["generationConfig"] = json!({"temperature": temp});
}
```

### Step 5: Register `fallback` module and re-export

In `lib.rs`:
```rust
pub mod fallback;
pub use fallback::FallbackAdapter;
```

### Step 6: Wire `FallbackAdapter` into kernel agent connect (optional)

Add a `--fallback-provider` flag to `agentctl agent connect` that creates a `FallbackAdapter` wrapping primary + fallback. This is optional and can be deferred to a later iteration.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/fallback.rs` | New file: `FallbackAdapter` |
| `crates/agentos-llm/src/lib.rs` | Add `pub mod fallback;`, re-export `FallbackAdapter` |
| `crates/agentos-llm/src/openai.rs` | Override `infer_with_options` with tool_choice, json_mode, temperature, seed |
| `crates/agentos-llm/src/anthropic.rs` | Override `infer_with_options` with tool_choice, temperature, max_tokens |
| `crates/agentos-llm/src/gemini.rs` | Override `infer_with_options` with functionCallingConfig, temperature |
| `crates/agentos-llm/src/ollama.rs` | Override `infer_with_options` with temperature (Ollama options) |

---

## Prerequisites

- [[05-retry-middleware-and-circuit-breaker]] (circuit breaker state informs failover decisions)
- [[06-cost-attribution-and-token-estimation]] (pricing is per-adapter)

---

## Test Plan

- Add test `test_fallback_uses_first_healthy` -- first provider succeeds, second is never called
- Add test `test_fallback_skips_unhealthy` -- first provider errors, second provider succeeds
- Add test `test_fallback_all_fail` -- both providers error, returns error
- Add test `test_openai_options_tool_choice_required` -- verify request body has `tool_choice: "required"`
- Add test `test_openai_options_json_mode` -- verify `response_format: {"type": "json_object"}` in body
- Add test `test_anthropic_options_tool_choice_specific` -- verify `tool_choice: {"type": "tool", "name": "..."}`

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
