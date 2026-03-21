use crate::traits::LLMCore;
use crate::types::{
    InferenceEvent, InferenceResult, InferenceToolCall, ModelCapabilities, TokenUsage,
};
use agentos_types::*;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub struct OllamaCore {
    client: Client,
    host: String,
    model: String,
    /// Context window size sent to Ollama as `num_ctx`. Configurable via `llm.ollama_context_window`.
    context_window: u32,
    capabilities: ModelCapabilities,
}

impl OllamaCore {
    /// Default context window size. Many modern Ollama models support 32K+.
    pub const DEFAULT_CONTEXT_WINDOW: u32 = 32768;

    /// Default HTTP request timeout. Cloud-proxied models may need much longer.
    pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 300;

    pub fn new(host: &str, model: &str) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(
                    Self::DEFAULT_REQUEST_TIMEOUT_SECS,
                ))
                .build()
                .expect("HTTP client TLS initialization failed"),
            host: host.to_string(),
            model: model.to_string(),
            context_window: Self::DEFAULT_CONTEXT_WINDOW,
            capabilities: ModelCapabilities {
                context_window_tokens: Self::DEFAULT_CONTEXT_WINDOW as u64,
                supports_images: false,
                supports_tool_calling: true,
                supports_json_mode: true,
                max_output_tokens: 0,
            },
        }
    }

    /// Override the HTTP request timeout for inference calls.
    ///
    /// Call this after construction to apply a value from kernel config
    /// (`ollama.request_timeout_secs`). Panics if `secs` is zero.
    pub fn with_request_timeout(mut self, secs: u64) -> Self {
        assert!(secs > 0, "request_timeout_secs must be greater than zero");
        self.client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(secs))
            .build()
            .expect("HTTP client TLS initialization failed");
        self
    }

    /// Override the context window size reported to callers and sent to Ollama as `num_ctx`.
    ///
    /// Call this after construction to apply a value from kernel config
    /// (`llm.ollama_context_window`). Panics if `tokens` is zero.
    pub fn with_context_window(mut self, tokens: u32) -> Self {
        assert!(
            tokens > 0,
            "context_window tokens must be greater than zero"
        );
        self.context_window = tokens;
        self.capabilities.context_window_tokens = tokens as u64;
        self
    }

    // --- Private helpers ---

    fn context_to_messages(&self, context: &ContextWindow) -> Vec<OllamaChatMessage> {
        context
            .active_entries()
            .iter()
            .map(|entry| OllamaChatMessage {
                role: match entry.role {
                    ContextRole::System => "system".to_string(),
                    ContextRole::User => "user".to_string(),
                    ContextRole::Assistant => "assistant".to_string(),
                    ContextRole::ToolResult => "user".to_string(),
                },
                content: entry.content.clone(),
                tool_calls: Vec::new(),
            })
            .collect()
    }

    async fn send_chat_request(
        &self,
        request: OllamaChatRequest,
    ) -> Result<OllamaChatResponse, AgentOSError> {
        let response = self
            .client
            .post(format!("{}/api/chat", self.host))
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                let mut reason = format!("Request failed: {}", e);
                let mut source = std::error::Error::source(&e);
                while let Some(s) = source {
                    reason += &format!(" -> {}", s);
                    source = std::error::Error::source(s);
                }
                AgentOSError::LLMError {
                    provider: "ollama".to_string(),
                    reason,
                }
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("API Error {}: {}", status, body),
            });
        }

        response
            .json::<OllamaChatResponse>()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Failed to parse JSON response: {}", e),
            })
    }

    fn response_to_inference_result(
        &self,
        ollama_response: OllamaChatResponse,
        duration_ms: u64,
    ) -> InferenceResult {
        let tool_calls: Vec<InferenceToolCall> = ollama_response
            .message
            .tool_calls
            .into_iter()
            .map(|tc| InferenceToolCall {
                id: None,
                tool_name: tc.function.name,
                intent_type: "execute".to_string(),
                payload: tc.function.arguments,
            })
            .collect();

        InferenceResult {
            text: ollama_response.message.content,
            tokens_used: TokenUsage {
                prompt_tokens: ollama_response.prompt_eval_count.unwrap_or(0),
                completion_tokens: ollama_response.eval_count.unwrap_or(0),
                total_tokens: ollama_response.prompt_eval_count.unwrap_or(0)
                    + ollama_response.eval_count.unwrap_or(0),
            },
            model: self.model.clone(),
            duration_ms,
            tool_calls,
            uncertainty: None,
        }
    }
}

// --- Ollama REST API types (private) ---

#[derive(Debug, Serialize)]
struct OllamaOptions {
    num_ctx: u32,
}

/// Tool function definition sent in requests (Ollama native tool calling).
#[derive(Debug, Serialize)]
struct OllamaRequestToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// Tool definition sent in requests.
#[derive(Debug, Serialize)]
struct OllamaRequestTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OllamaRequestToolFunction,
}

/// Tool call function returned in assistant messages.
#[derive(Debug, Deserialize)]
struct OllamaResponseToolCallFunction {
    name: String,
    arguments: serde_json::Value,
}

/// Native tool call returned by the model in a response message.
#[derive(Debug, Deserialize)]
struct OllamaResponseToolCall {
    function: OllamaResponseToolCallFunction,
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaChatMessage>,
    stream: bool,
    options: OllamaOptions,
    /// Tool definitions — omitted when empty so non-tool requests stay minimal.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OllamaRequestTool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
    /// Native tool calls emitted by the model. Only present in assistant responses;
    /// skipped when serializing outgoing request messages.
    #[serde(default, skip_serializing)]
    tool_calls: Vec<OllamaResponseToolCall>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OllamaChatResponse {
    model: String,
    message: OllamaChatMessage,
    done: bool,
    total_duration: Option<u64>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
}

#[async_trait]
impl LLMCore for OllamaCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let start = std::time::Instant::now();

        // Convert ContextWindow to Ollama chat messages format
        let messages = self.context_to_messages(context);

        let request = OllamaChatRequest {
            model: self.model.clone(),
            messages,
            stream: false,
            options: OllamaOptions {
                num_ctx: self.context_window,
            },
            tools: Vec::new(),
        };

        let ollama_response = self.send_chat_request(request).await?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(self.response_to_inference_result(ollama_response, duration_ms))
    }

    async fn infer_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
    ) -> Result<InferenceResult, AgentOSError> {
        let start = std::time::Instant::now();
        let messages = self.context_to_messages(context);
        let ollama_tools = tools
            .iter()
            .map(|t| OllamaRequestTool {
                tool_type: "function".to_string(),
                function: OllamaRequestToolFunction {
                    name: t.manifest.name.clone(),
                    description: t.manifest.description.clone(),
                    parameters: t
                        .input_schema
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
                },
            })
            .collect();

        let request = OllamaChatRequest {
            model: self.model.clone(),
            messages,
            stream: false,
            options: OllamaOptions {
                num_ctx: self.context_window,
            },
            tools: ollama_tools,
        };

        let ollama_response = self.send_chat_request(request).await?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(self.response_to_inference_result(ollama_response, duration_ms))
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        use crate::types::HealthStatus;
        let start = std::time::Instant::now();
        match self
            .client
            .get(format!("{}/api/tags", self.host))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let latency = start.elapsed();
                if latency > std::time::Duration::from_secs(2) {
                    HealthStatus::Degraded {
                        reason: format!("High latency: {}ms", latency.as_millis()),
                    }
                } else {
                    HealthStatus::Healthy
                }
            }
            Ok(resp) => HealthStatus::Unhealthy {
                reason: format!("HTTP {}", resp.status()),
            },
            Err(e) => HealthStatus::Unhealthy {
                reason: format!("Connection failed: {e}"),
            },
        }
    }

    async fn infer_stream(
        &self,
        context: &ContextWindow,
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        let start = std::time::Instant::now();

        let messages = self.context_to_messages(context);

        let request = OllamaChatRequest {
            model: self.model.clone(),
            messages,
            stream: true,
            options: OllamaOptions {
                num_ctx: self.context_window,
            },
            tools: Vec::new(),
        };

        let response = self
            .client
            .post(format!("{}/api/chat", self.host))
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                let mut reason = format!("Request failed: {}", e);
                let mut source = std::error::Error::source(&e);
                while let Some(s) = source {
                    reason += &format!(" -> {}", s);
                    source = std::error::Error::source(s);
                }
                AgentOSError::LLMError {
                    provider: "ollama".to_string(),
                    reason,
                }
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let err = AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("API Error {}: {}", status, body),
            };
            let _ = tx.send(InferenceEvent::Error(err.to_string())).await;
            return Err(err);
        }

        let mut full_text = String::new();
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;

        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;

            // Ollama sends newline-delimited JSON
            for line in chunk.split(|&b| b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                if let Ok(resp) = serde_json::from_slice::<OllamaChatResponse>(line) {
                    if !resp.message.content.is_empty() {
                        full_text.push_str(&resp.message.content);
                        let _ = tx.send(InferenceEvent::Token(resp.message.content)).await;
                    }
                    if resp.done {
                        prompt_tokens = resp.prompt_eval_count.unwrap_or(0);
                        completion_tokens = resp.eval_count.unwrap_or(0);
                    }
                }
            }
        }

        let result = InferenceResult {
            text: full_text,
            tokens_used: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
            model: self.model.clone(),
            duration_ms: start.elapsed().as_millis() as u64,
            tool_calls: Vec::new(),
            uncertainty: None,
        };
        let _ = tx.send(InferenceEvent::Done(result)).await;
        Ok(())
    }

    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

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
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello!".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let entries = ctx.as_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, ContextRole::System);
        assert_eq!(entries[1].role, ContextRole::User);
    }

    #[test]
    fn test_default_context_window() {
        let adapter = OllamaCore::new("http://localhost:11434", "llama3.2");
        assert_eq!(adapter.context_window, OllamaCore::DEFAULT_CONTEXT_WINDOW);
        assert_eq!(
            adapter.capabilities().context_window_tokens,
            OllamaCore::DEFAULT_CONTEXT_WINDOW as u64
        );
    }

    #[test]
    fn test_with_context_window_updates_field_and_capabilities() {
        let adapter =
            OllamaCore::new("http://localhost:11434", "llama3.2").with_context_window(131072);
        assert_eq!(adapter.context_window, 131072);
        assert_eq!(adapter.capabilities().context_window_tokens, 131072);
    }

    #[test]
    #[should_panic(expected = "context_window tokens must be greater than zero")]
    fn test_with_context_window_rejects_zero() {
        let _ = OllamaCore::new("http://localhost:11434", "llama3.2").with_context_window(0);
    }

    #[tokio::test]
    #[ignore] // only run when Ollama is available
    async fn test_ollama_health_check() {
        let ollama = OllamaCore::new("http://localhost:11434", "llama3.2");
        let status = ollama.health_check().await;
        assert!(
            status.is_healthy(),
            "Ollama should be running on localhost:11434"
        );
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
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let result = ollama.infer(&ctx).await.unwrap();
        assert!(!result.text.is_empty());
        assert!(result.tokens_used.total_tokens > 0);
    }
}
