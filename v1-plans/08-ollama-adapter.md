# Plan 08 — Ollama Adapter (`agentos-llm` crate)

## Goal

Implement the `LLMCore` trait and an Ollama adapter that connects to a local Ollama instance via its REST API. This is the only LLM backend in Phase 1.

## Dependencies

- `agentos-types`
- `async-trait`
- `reqwest` (with `json` feature)
- `serde`, `serde_json`
- `tracing`

## LLMCore Trait Definition

```rust
// In src/traits.rs
use agentos_types::*;
use async_trait::async_trait;

#[async_trait]
pub trait LLMCore: Send + Sync {
    /// Send a context window to the LLM and get a response.
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError>;

    /// Get the model's capabilities (context window size, etc.)
    fn capabilities(&self) -> &ModelCapabilities;

    /// Check if the LLM backend is reachable and healthy.
    async fn health_check(&self) -> bool;

    /// Get the provider name (for display/logging).
    fn provider_name(&self) -> &str;

    /// Get the model name.
    fn model_name(&self) -> &str;
}
```

## Types

```rust
// In src/types.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    pub text: String,
    pub tokens_used: TokenUsage,
    pub model: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub context_window_tokens: u64,
    pub supports_images: bool,
    pub supports_tool_calling: bool,
    pub supports_json_mode: bool,
}
```

## Ollama Adapter Implementation

```rust
// In src/ollama.rs
use reqwest::Client;

pub struct OllamaCore {
    client: Client,
    host: String,         // e.g. "http://localhost:11434"
    model: String,        // e.g. "llama3.2"
    capabilities: ModelCapabilities,
}

impl OllamaCore {
    pub fn new(host: &str, model: &str) -> Self {
        Self {
            client: Client::new(),
            host: host.to_string(),
            model: model.to_string(),
            capabilities: ModelCapabilities {
                context_window_tokens: 8192,  // default, updated on health check
                supports_images: false,
                supports_tool_calling: false,
                supports_json_mode: true,
            },
        }
    }
}

#[async_trait]
impl LLMCore for OllamaCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let start = std::time::Instant::now();

        // Convert ContextWindow to Ollama chat messages format
        let messages: Vec<OllamaChatMessage> = context.as_entries()
            .iter()
            .map(|entry| OllamaChatMessage {
                role: match entry.role {
                    ContextRole::System => "system".to_string(),
                    ContextRole::User => "user".to_string(),
                    ContextRole::Assistant => "assistant".to_string(),
                    ContextRole::ToolResult => "user".to_string(), // tool results sent as user messages
                },
                content: entry.content.clone(),
            })
            .collect();

        let request = OllamaChatRequest {
            model: self.model.clone(),
            messages,
            stream: false,
        };

        let response = self.client
            .post(format!("{}/api/chat", self.host))
            .json(&request)
            .send()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "ollama".into(),
                reason: format!("HTTP request failed: {}", e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "ollama".into(),
                reason: format!("HTTP {}: {}", status, body),
            });
        }

        let ollama_response: OllamaChatResponse = response.json().await
            .map_err(|e| AgentOSError::LLMError {
                provider: "ollama".into(),
                reason: format!("Failed to parse response: {}", e),
            })?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(InferenceResult {
            text: ollama_response.message.content,
            tokens_used: TokenUsage {
                prompt_tokens: ollama_response.prompt_eval_count.unwrap_or(0),
                completion_tokens: ollama_response.eval_count.unwrap_or(0),
                total_tokens: ollama_response.prompt_eval_count.unwrap_or(0)
                    + ollama_response.eval_count.unwrap_or(0),
            },
            model: self.model.clone(),
            duration_ms,
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> bool {
        match self.client.get(format!("{}/api/tags", self.host)).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
```

## Ollama API Types

```rust
// Ollama REST API request/response types (private to this module)

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaChatMessage>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    model: String,
    message: OllamaChatMessage,
    done: bool,
    total_duration: Option<u64>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
}
```

## Module Exports

```rust
// src/lib.rs
pub mod traits;
pub mod types;
pub mod ollama;

pub use traits::LLMCore;
pub use types::{InferenceResult, TokenUsage, ModelCapabilities};
pub use ollama::OllamaCore;
```

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_to_messages_conversion() {
        let mut ctx = ContextWindow::new(100);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "You are a helpful assistant.".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello!".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });

        // Verify the conversion produces the right roles
        let entries = ctx.as_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, ContextRole::System);
        assert_eq!(entries[1].role, ContextRole::User);
    }

    // Integration test — requires running Ollama instance
    // Run with: cargo test -p agentos-llm -- --ignored
    #[tokio::test]
    #[ignore] // only run when Ollama is available
    async fn test_ollama_health_check() {
        let ollama = OllamaCore::new("http://localhost:11434", "llama3.2");
        let healthy = ollama.health_check().await;
        assert!(healthy, "Ollama should be running on localhost:11434");
    }

    #[tokio::test]
    #[ignore]
    async fn test_ollama_infer() {
        let ollama = OllamaCore::new("http://localhost:11434", "llama3.2");

        let mut ctx = ContextWindow::new(100);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Say 'hello' and nothing else.".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });

        let result = ollama.infer(&ctx).await.unwrap();
        assert!(!result.text.is_empty());
        assert!(result.tokens_used.total_tokens > 0);
    }
}
```

## Verification

```bash
# Unit tests (no Ollama required)
cargo test -p agentos-llm

# Integration tests (requires Ollama running with llama3.2)
# First: ollama pull llama3.2
# Then:
cargo test -p agentos-llm -- --ignored
```
