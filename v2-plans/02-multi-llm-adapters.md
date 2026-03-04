# Plan 02 — Multi-LLM Adapters & Task Routing

## Goal

Implement adapters for OpenAI, Anthropic, and Gemini APIs alongside the existing Ollama adapter. Build a **task routing engine** that selects the best LLM for each task based on configurable policies (capability-first, cost-first, latency-first, round-robin).

## Dependencies

- `agentos-types`
- `agentos-llm` (extend existing crate)
- `reqwest` (already in workspace)
- `async-trait` (already in workspace)
- `serde`, `serde_json`
- `tracing`

## New LLM Adapters

### OpenAI Adapter

```rust
// In agentos-llm/src/openai.rs

pub struct OpenAICore {
    client: Client,
    api_key: ZeroizingString,    // from secrets vault
    model: String,               // gpt-4o, o1, etc.
    base_url: String,            // https://api.openai.com/v1 (or custom)
    capabilities: ModelCapabilities,
}

impl OpenAICore {
    pub fn new(api_key: &str, model: &str) -> Self;
    pub fn with_base_url(api_key: &str, model: &str, base_url: &str) -> Self;
}

#[async_trait]
impl LLMCore for OpenAICore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError>;
    fn capabilities(&self) -> &ModelCapabilities;
    async fn health_check(&self) -> bool;
    fn provider_name(&self) -> &str;  // "openai"
    fn model_name(&self) -> &str;
}
```

REST API: **POST** `{base_url}/chat/completions` with `messages` array, `model`, `stream: false`.

### Anthropic Adapter

```rust
// In agentos-llm/src/anthropic.rs

pub struct AnthropicCore {
    client: Client,
    api_key: ZeroizingString,
    model: String,               // claude-sonnet-4, etc.
    capabilities: ModelCapabilities,
}
```

REST API: **POST** `https://api.anthropic.com/v1/messages` with `messages`, `model`, `max_tokens`, header `x-api-key` and `anthropic-version: 2023-06-01`.

### Gemini Adapter

```rust
// In agentos-llm/src/gemini.rs

pub struct GeminiCore {
    client: Client,
    api_key: ZeroizingString,
    model: String,               // gemini-1.5-pro, etc.
    capabilities: ModelCapabilities,
}
```

REST API: **POST** `https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}` with `contents` array.

### Custom/OpenAI-Compatible Adapter

```rust
// In agentos-llm/src/custom.rs

pub struct CustomCore {
    client: Client,
    api_key: Option<ZeroizingString>,
    model: String,
    base_url: String,             // e.g. http://localhost:8000/v1
    capabilities: ModelCapabilities,
}
```

Any OpenAI-compatible endpoint (vLLM, llama.cpp server, LMStudio, etc.).

## LLM Provider Enum Update

```rust
// Update agentos-types/src/agent.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LLMProvider {
    Ollama,
    OpenAI,
    Anthropic,
    Gemini,
    Custom(String),   // custom provider name
}
```

## Task Routing Engine

```rust
// In agentos-kernel/src/router.rs

pub struct TaskRouter {
    agents: Vec<AgentID>,
    strategy: RoutingStrategy,
    rules: Vec<RoutingRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub enum RoutingStrategy {
    CapabilityFirst,   // pick the most capable model
    CostFirst,         // pick the cheapest model
    LatencyFirst,      // pick the fastest model
    RoundRobin,        // distribute evenly
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutingRule {
    pub task_pattern: Option<String>,    // regex match on prompt
    pub preferred_agent: String,         // agent name
    pub fallback_agent: Option<String>,  // if preferred is unavailable
}

impl TaskRouter {
    pub fn new(strategy: RoutingStrategy, rules: Vec<RoutingRule>) -> Self;

    /// Select the best agent for a task.
    /// Returns the AgentID of the selected agent.
    pub async fn route(
        &self,
        prompt: &str,
        agents: &[AgentProfile],
    ) -> Result<AgentID, AgentOSError>;
}
```

## Config Changes

```toml
# config/default.toml additions

[routing]
strategy = "capability-first"   # capability-first | cost-first | latency-first | round-robin

[[routing.rules]]
task_pattern    = ".*code.*"
preferred_agent = "anthropic-coder"
fallback_agent  = "ollama-local"

[[routing.rules]]
task_pattern    = ".*summarize.*"
preferred_agent = "ollama-local"
```

## Kernel Changes

```rust
// Kernel needs to:
// 1. Store API keys in vault (not as env vars)
// 2. Create the right adapter type based on LLMProvider
// 3. Use TaskRouter to select agent for tasks submitted without --agent
// 4. Store per-adapter instances in a HashMap<AgentID, Box<dyn LLMCore>>
```

The `cmd_connect_agent` handler is updated to:

1. Parse `LLMProvider` from the provider string
2. Retrieve the API key from the vault (for cloud providers)
3. Instantiate the correct adapter
4. Register in the agent registry with LLM adapter reference

## CLI Changes

```bash
# Connect OpenAI (prompts for API key interactively)
agentctl agent connect --provider openai --model gpt-4o --name researcher

# Connect Anthropic
agentctl agent connect --provider anthropic --model claude-sonnet-4 --name coder

# Connect custom OpenAI-compatible
agentctl agent connect --provider custom --model my-model --name local-llm \
  --base-url http://localhost:8000/v1

# Run task with auto-routing (no --agent flag)
agentctl task run "Summarize the error logs"
# → Router selects best available agent based on strategy + rules
```

## Module Exports

```rust
// agentos-llm/src/lib.rs
pub mod traits;
pub mod types;
pub mod ollama;
pub mod openai;
pub mod anthropic;
pub mod gemini;
pub mod custom;

pub use traits::LLMCore;
pub use types::{InferenceResult, TokenUsage, ModelCapabilities};
pub use ollama::OllamaCore;
pub use openai::OpenAICore;
pub use anthropic::AnthropicCore;
pub use gemini::GeminiCore;
pub use custom::CustomCore;
```

## Tests

```rust
#[cfg(test)]
mod tests {
    // Unit tests (no API keys needed):

    #[test]
    fn test_openai_message_format() {
        // Verify ContextWindow → OpenAI messages array conversion
    }

    #[test]
    fn test_anthropic_message_format() {
        // Verify ContextWindow → Anthropic messages array conversion
        // Note: Anthropic separates system prompt from messages
    }

    #[test]
    fn test_gemini_message_format() {
        // Verify ContextWindow → Gemini contents array conversion
    }

    #[test]
    fn test_routing_capability_first() {
        // With 2 agents (one high-capability, one low), verify routing picks the better one
    }

    #[test]
    fn test_routing_round_robin() {
        // Verify requests are distributed evenly
    }

    #[test]
    fn test_routing_rule_pattern_match() {
        // Verify task_pattern regex matches route to preferred_agent
    }

    // Integration tests (require API keys):
    #[tokio::test]
    #[ignore]
    async fn test_openai_health_check() { /* ... */ }

    #[tokio::test]
    #[ignore]
    async fn test_anthropic_health_check() { /* ... */ }
}
```

## Verification

```bash
# Unit tests
cargo test -p agentos-llm

# Integration tests (requires API keys set in vault)
cargo test -p agentos-llm -- --ignored
```
