use crate::traits::LLMCore;
use crate::types::{
    calculate_inference_cost, default_pricing_table, InferenceEvent, InferenceOptions,
    InferenceResult, InferenceToolCall, ModelCapabilities, ModelPricing, StopReason, TokenUsage,
    ToolChoice,
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
    pricing: ModelPricing,
    retry_policy: crate::retry::RetryPolicy,
    circuit_breaker: crate::retry::CircuitBreaker,
}

impl OllamaCore {
    /// Default context window size. Many modern Ollama models support 32K+.
    pub const DEFAULT_CONTEXT_WINDOW: u32 = 32768;

    /// Default HTTP request timeout. Cloud-proxied models may need much longer.
    pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 300;

    pub fn new(host: &str, model: &str) -> Self {
        // Ollama wildcard entry has zero-cost (local inference).
        let table = default_pricing_table();
        let pricing = table
            .iter()
            .find(|p| p.provider == "ollama" && p.model == model)
            .or_else(|| {
                table
                    .iter()
                    .find(|p| p.provider == "ollama" && p.model == "*")
            })
            .cloned()
            .unwrap_or(ModelPricing {
                provider: "ollama".to_string(),
                model: model.to_string(),
                input_per_1k: 0.0,
                output_per_1k: 0.0,
            });
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
                supports_streaming: true,
                supports_parallel_tools: false,
                supports_prompt_caching: false,
                supports_thinking: false,
                supports_structured_output: false,
            },
            pricing,
            retry_policy: crate::retry::RetryPolicy::default(),
            circuit_breaker: crate::retry::CircuitBreaker::default(),
        }
    }

    /// Override the pricing for this adapter instance.
    pub fn with_pricing(mut self, pricing: ModelPricing) -> Self {
        self.pricing = pricing;
        self
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
        use serde_json::Value;
        context
            .active_entries()
            .iter()
            .map(|entry| {
                match entry.role {
                    ContextRole::Assistant => {
                        // Reconstruct Ollama/OpenAI-compatible tool_calls array for
                        // multi-turn contexts where the assistant made tool calls.
                        // Without this, Ollama sees an assistant message followed by
                        // tool results with no matching tool_calls declaration.
                        let request_tool_calls = entry
                            .metadata
                            .as_ref()
                            .and_then(|m| m.assistant_tool_calls.as_ref())
                            .and_then(|v| v.as_array())
                            .map(|calls| {
                                calls
                                    .iter()
                                    .filter_map(|call| {
                                        let name = call.get("tool_name")?.as_str()?;
                                        let args = call
                                            .get("payload")
                                            .cloned()
                                            .unwrap_or_else(|| serde_json::json!({}));
                                        Some(serde_json::json!({
                                            "function": {"name": name, "arguments": args}
                                        }))
                                    })
                                    .collect::<Vec<Value>>()
                            })
                            .filter(|v: &Vec<Value>| !v.is_empty());
                        OllamaChatMessage {
                            role: "assistant".to_string(),
                            content: entry.content.clone(),
                            tool_calls: Vec::new(),
                            request_tool_calls,
                        }
                    }
                    ContextRole::ToolResult => {
                        // Use native "tool" role if we have a tool_call_id.
                        let (role, content) = if entry
                            .metadata
                            .as_ref()
                            .and_then(|m| m.tool_call_id.as_deref())
                            .is_some()
                        {
                            ("tool".to_string(), entry.content.clone())
                        } else {
                            (
                                "user".to_string(),
                                format!("Tool Result:\n{}", entry.content),
                            )
                        };
                        OllamaChatMessage {
                            role,
                            content,
                            tool_calls: Vec::new(),
                            request_tool_calls: None,
                        }
                    }
                    ContextRole::System => OllamaChatMessage {
                        role: "system".to_string(),
                        content: entry.content.clone(),
                        tool_calls: Vec::new(),
                        request_tool_calls: None,
                    },
                    ContextRole::User => OllamaChatMessage {
                        role: "user".to_string(),
                        content: entry.content.clone(),
                        tool_calls: Vec::new(),
                        request_tool_calls: None,
                    },
                }
            })
            .collect()
    }

    async fn send_chat_request(
        &self,
        request: OllamaChatRequest,
    ) -> Result<OllamaChatResponse, AgentOSError> {
        let url = format!("{}/api/chat", self.host);
        let response = crate::retry::send_with_retry(
            "ollama",
            &self.retry_policy,
            &self.circuit_breaker,
            || self.client.post(&url).json(&request),
        )
        .await?;

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

        let stop_reason = if !tool_calls.is_empty() {
            StopReason::ToolUse
        } else {
            match ollama_response.done_reason.as_deref() {
                Some("length") => StopReason::MaxTokens,
                Some("stop") | None => StopReason::EndTurn,
                Some(other) => StopReason::Other(other.to_string()),
            }
        };

        let tokens_used = TokenUsage {
            prompt_tokens: ollama_response.prompt_eval_count.unwrap_or(0),
            completion_tokens: ollama_response.eval_count.unwrap_or(0),
            total_tokens: ollama_response.prompt_eval_count.unwrap_or(0)
                + ollama_response.eval_count.unwrap_or(0),
        };
        let cost = calculate_inference_cost(&tokens_used, &self.pricing);

        InferenceResult {
            text: ollama_response.message.content,
            tokens_used,
            model: self.model.clone(),
            duration_ms,
            tool_calls,
            uncertainty: None,
            stop_reason,
            cost: Some(cost),
            cached_tokens: 0,
        }
    }
}

// --- Ollama REST API types (private) ---

#[derive(Debug, Serialize)]
struct OllamaOptions {
    num_ctx: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
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
    /// Response format — set to "json" for JSON mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
    /// Inbound tool calls deserialized from model responses. Never serialized
    /// outbound (use `request_tool_calls` for that instead).
    #[serde(default, skip_serializing)]
    tool_calls: Vec<OllamaResponseToolCall>,
    /// Outbound tool calls for prior assistant messages in multi-turn context.
    /// Serialized as `"tool_calls"` (Ollama/OpenAI-compatible format); skipped
    /// when None so non-tool-call messages stay minimal.
    #[serde(
        rename = "tool_calls",
        skip_serializing_if = "Option::is_none",
        skip_deserializing
    )]
    request_tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OllamaChatResponse {
    model: String,
    message: OllamaChatMessage,
    done: bool,
    /// Why the model stopped generating (e.g. "stop", "length").
    #[serde(default)]
    done_reason: Option<String>,
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
                temperature: None,
            },
            tools: Vec::new(),
            format: None,
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
        self.infer_with_options(context, tools, &InferenceOptions::default())
            .await
    }

    async fn infer_with_options(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        options: &InferenceOptions,
    ) -> Result<InferenceResult, AgentOSError> {
        let start = std::time::Instant::now();
        let messages = self.context_to_messages(context);

        // If options disable tools, exclude them from the request.
        let effective_tools = if matches!(options.tool_choice, Some(ToolChoice::None)) {
            &[][..]
        } else {
            tools
        };
        let ollama_tools = effective_tools
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
                temperature: options.temperature,
            },
            tools: ollama_tools,
            format: if options.json_mode {
                Some("json".to_string())
            } else {
                None
            },
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
                temperature: None,
            },
            tools: Vec::new(),
            format: None,
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
        let mut done_reason: Option<String> = None;
        let mut tool_calls: Vec<InferenceToolCall> = Vec::new();

        let mut line_buf: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            line_buf.extend_from_slice(&chunk);

            // Process complete NDJSON lines from the buffer.
            while let Some(newline_pos) = line_buf.iter().position(|&b| b == b'\n') {
                let line = &line_buf[..newline_pos];
                if !line.is_empty() {
                    if let Ok(resp) = serde_json::from_slice::<OllamaChatResponse>(line) {
                        if !resp.message.content.is_empty() {
                            full_text.push_str(&resp.message.content);
                            let _ = tx.send(InferenceEvent::Token(resp.message.content)).await;
                        }
                        if resp.done {
                            prompt_tokens = resp.prompt_eval_count.unwrap_or(0);
                            completion_tokens = resp.eval_count.unwrap_or(0);
                            done_reason = resp.done_reason;

                            for tc in &resp.message.tool_calls {
                                let itc = InferenceToolCall {
                                    id: None,
                                    tool_name: tc.function.name.clone(),
                                    intent_type: "execute".to_string(),
                                    payload: tc.function.arguments.clone(),
                                };
                                let _ =
                                    tx.send(InferenceEvent::ToolCallComplete(itc.clone())).await;
                                tool_calls.push(itc);
                            }
                        }
                    }
                }
                line_buf = line_buf[newline_pos + 1..].to_vec();
            }
        }

        let stop_reason = if !tool_calls.is_empty() {
            StopReason::ToolUse
        } else {
            match done_reason.as_deref() {
                Some("length") => StopReason::MaxTokens,
                Some("stop") | None => StopReason::EndTurn,
                Some(other) => StopReason::Other(other.to_string()),
            }
        };

        let tokens_used = TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        };
        let cost = calculate_inference_cost(&tokens_used, &self.pricing);
        let result = InferenceResult {
            text: full_text,
            tokens_used,
            model: self.model.clone(),
            duration_ms: start.elapsed().as_millis() as u64,
            tool_calls,
            uncertainty: None,
            stop_reason,
            cost: Some(cost),
            cached_tokens: 0,
        };
        let _ = tx.send(InferenceEvent::Done(result)).await;
        Ok(())
    }

    async fn infer_stream_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        let start = std::time::Instant::now();
        let messages = self.context_to_messages(context);
        let ollama_tools: Vec<OllamaRequestTool> = tools
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
            stream: true,
            options: OllamaOptions {
                num_ctx: self.context_window,
                temperature: None,
            },
            tools: ollama_tools,
            format: None,
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
        let mut done_reason: Option<String> = None;
        let mut tool_calls: Vec<InferenceToolCall> = Vec::new();

        let mut line_buf: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            line_buf.extend_from_slice(&chunk);

            while let Some(newline_pos) = line_buf.iter().position(|&b| b == b'\n') {
                let line = &line_buf[..newline_pos];
                if !line.is_empty() {
                    if let Ok(resp) = serde_json::from_slice::<OllamaChatResponse>(line) {
                        if !resp.message.content.is_empty() {
                            full_text.push_str(&resp.message.content);
                            let _ = tx.send(InferenceEvent::Token(resp.message.content)).await;
                        }
                        if resp.done {
                            prompt_tokens = resp.prompt_eval_count.unwrap_or(0);
                            completion_tokens = resp.eval_count.unwrap_or(0);
                            done_reason = resp.done_reason;

                            for tc in &resp.message.tool_calls {
                                let itc = InferenceToolCall {
                                    id: None,
                                    tool_name: tc.function.name.clone(),
                                    intent_type: "execute".to_string(),
                                    payload: tc.function.arguments.clone(),
                                };
                                let _ =
                                    tx.send(InferenceEvent::ToolCallComplete(itc.clone())).await;
                                tool_calls.push(itc);
                            }
                        }
                    }
                }
                line_buf = line_buf[newline_pos + 1..].to_vec();
            }
        }

        let stop_reason = if !tool_calls.is_empty() {
            StopReason::ToolUse
        } else {
            match done_reason.as_deref() {
                Some("length") => StopReason::MaxTokens,
                Some("stop") | None => StopReason::EndTurn,
                Some(other) => StopReason::Other(other.to_string()),
            }
        };

        let tokens_used = TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        };
        let cost = calculate_inference_cost(&tokens_used, &self.pricing);
        let result = InferenceResult {
            text: full_text,
            tokens_used,
            model: self.model.clone(),
            duration_ms: start.elapsed().as_millis() as u64,
            tool_calls,
            uncertainty: None,
            stop_reason,
            cost: Some(cost),
            cached_tokens: 0,
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

    #[test]
    fn test_context_to_messages_native_tool_result() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "tool output".to_string(),
            metadata: Some(ContextMetadata {
                tool_name: Some("shell".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("call_xyz".to_string()),
                assistant_tool_calls: None,
            }),
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = OllamaCore::new("http://localhost:11434", "llama3.2");
        let messages = adapter.context_to_messages(&ctx);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "tool");
        assert_eq!(messages[0].content, "tool output");
    }

    #[test]
    fn test_context_to_messages_legacy_tool_result() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "tool output".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = OllamaCore::new("http://localhost:11434", "llama3.2");
        let messages = adapter.context_to_messages(&ctx);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Tool Result:\ntool output");
    }
}
