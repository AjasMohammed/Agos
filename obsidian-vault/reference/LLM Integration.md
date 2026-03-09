---
title: LLM Integration
tags: [reference, llm]
---

# LLM Integration

AgentOS supports multiple LLM providers through a trait-based adapter pattern. The kernel is LLM-agnostic - any provider implementing `LLMCore` can be used.

**Source:** `crates/agentos-llm/src/`

## LLMCore Trait

```rust
#[async_trait]
pub trait LLMCore: Send + Sync {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError>;
    fn capabilities(&self) -> &ModelCapabilities;
    async fn health_check(&self) -> bool;
    fn provider_name(&self) -> &str;
    fn model_name(&self) -> &str;
}
```

## Supported Providers

### Ollama (Local)
- Default: `http://localhost:11434`
- Models: llama3.2, codellama, mistral, etc.
- No API key needed

### OpenAI
- Models: gpt-4, gpt-3.5-turbo, etc.
- Requires API key (stored in [[Vault and Secrets|vault]])

### Anthropic
- Models: Claude family
- Requires API key

### Gemini
- Models: gemini-pro, etc.
- Requires API key

### Custom
- Any HTTP endpoint implementing a compatible API
- Configurable base URL

## Inference Result

```rust
pub struct InferenceResult {
    pub content: String,
    pub stop_reason: String,       // "end_turn", "max_tokens", "tool_use"
    pub tokens_used: TokenUsage,   // { input: u64, output: u64 }
    pub metadata: Option<Value>,
}
```

## Model Capabilities

```rust
pub struct ModelCapabilities {
    pub context_window: usize,     // max tokens
    pub max_output_tokens: usize,
    pub vision_capable: bool,
    pub tool_use_capable: bool,
}
```

Used by the [[Kernel Deep Dive#Task Router|Task Router]] for capability-first routing.

## Context Window

The `ContextWindow` passed to `infer()` contains:

```rust
pub struct ContextWindow {
    pub entries: Vec<ContextEntry>,
    pub max_entries: usize,
}

pub struct ContextEntry {
    pub role: ContextRole,    // System | User | Assistant | ToolResult
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub metadata: Option<ContextMetadata>,
}
```

## Multi-Agent Setup

Multiple agents with different providers can run simultaneously:

```bash
agentctl agent connect --provider ollama --model llama3.2 --name local-agent
agentctl agent connect --provider openai --model gpt-4 --name cloud-agent
agentctl agent connect --provider anthropic --model claude-sonnet-4-20250514 --name claude-agent
```

The kernel's [[Kernel Deep Dive#Task Router|router]] can automatically select the best agent based on the task requirements.
