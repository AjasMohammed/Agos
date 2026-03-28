use crate::tool_helpers;
use crate::traits::LLMCore;
use crate::types::{
    calculate_inference_cost, default_pricing_table, InferenceEvent, InferenceOptions,
    InferenceResult, InferenceToolCall, ModelCapabilities, ModelPricing, StopReason, TokenUsage,
    ToolChoice,
};
use agentos_types::*;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::warn;

/// OpenAI API adapter for models like gpt-4o, gpt-3.5-turbo, etc.
pub struct OpenAICore {
    client: Client,
    api_key: SecretString,
    model: String,
    base_url: String,
    capabilities: ModelCapabilities,
    pricing: ModelPricing,
    retry_policy: crate::retry::RetryPolicy,
    circuit_breaker: crate::retry::CircuitBreaker,
}

impl OpenAICore {
    /// Create a new OpenAI adapter using the default base URL.
    pub fn new(api_key: SecretString, model: String) -> Self {
        Self::with_base_url(api_key, model, "https://api.openai.com/v1".to_string())
    }

    /// Create a new OpenAI adapter with a custom base URL.
    pub fn with_base_url(api_key: SecretString, model: String, base_url: String) -> Self {
        let table = default_pricing_table();
        let pricing = table
            .iter()
            .find(|p| p.provider == "openai" && p.model == model)
            .or_else(|| {
                table
                    .iter()
                    .find(|p| p.provider == "openai" && p.model == "*")
            })
            .cloned()
            .unwrap_or(ModelPricing {
                provider: "openai".to_string(),
                model: model.clone(),
                input_per_1k: 0.0,
                output_per_1k: 0.0,
            });
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("HTTP client TLS initialization failed"),
            api_key,
            model,
            base_url,
            capabilities: ModelCapabilities {
                context_window_tokens: 128_000,
                supports_images: true,
                supports_tool_calling: true,
                supports_json_mode: true,
                max_output_tokens: 0,
                supports_streaming: true,
                supports_parallel_tools: true,
                supports_prompt_caching: false,
                supports_thinking: false,
                supports_structured_output: true,
            },
            pricing,
            retry_policy: crate::retry::RetryPolicy::default(),
            circuit_breaker: crate::retry::CircuitBreaker::default(),
        }
    }

    /// Override the pricing for this adapter instance (e.g., for custom deployments).
    pub fn with_pricing(mut self, pricing: ModelPricing) -> Self {
        self.pricing = pricing;
        self
    }

    /// Convert our internal `ContextWindow` to OpenAI's messages array format.
    fn format_messages(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();

        for entry in context.active_entries() {
            match entry.role {
                ContextRole::ToolResult => {
                    // Check for native tool result metadata (provider tool_call_id).
                    let tool_call_id = entry
                        .metadata
                        .as_ref()
                        .and_then(|m| m.tool_call_id.as_deref());

                    if let Some(call_id) = tool_call_id {
                        // Native OpenAI tool result: role "tool" with tool_call_id.
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": entry.content,
                        }));
                    } else {
                        // Legacy fallback: plain user message with prefix.
                        messages.push(json!({
                            "role": "user",
                            "content": format!("Tool Result:\n{}", entry.content),
                        }));
                    }
                }
                ContextRole::System => {
                    messages.push(json!({
                        "role": "system",
                        "content": entry.content,
                    }));
                }
                ContextRole::User => {
                    messages.push(json!({
                        "role": "user",
                        "content": entry.content,
                    }));
                }
                ContextRole::Assistant => {
                    // If this assistant turn invoked tools, reconstruct the
                    // OpenAI-native format: {"role":"assistant","tool_calls":[...]}.
                    // OpenAI requires the preceding assistant message to contain the
                    // tool_calls array with matching IDs before any role:"tool" message.
                    if let Some(Value::Array(calls)) = entry
                        .metadata
                        .as_ref()
                        .and_then(|m| m.assistant_tool_calls.as_ref())
                    {
                        let openai_tool_calls: Vec<Value> = calls
                            .iter()
                            .enumerate()
                            .filter_map(|(idx, call)| {
                                let name = call.get("tool_name")?.as_str()?;
                                // Use provider-native ID if available; fall back to a
                                // unique positional ID so parallel tool calls each get
                                // a distinct ID that the matching role:tool messages can
                                // reference (OpenAI rejects duplicate IDs in one turn).
                                let id = call
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| format!("call_{idx}"));
                                let args = call
                                    .get("payload")
                                    .cloned()
                                    .unwrap_or_else(|| json!({}))
                                    .to_string();
                                Some(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {"name": name, "arguments": args},
                                }))
                            })
                            .collect();
                        let content = if entry.content.is_empty() {
                            Value::Null
                        } else {
                            Value::String(entry.content.clone())
                        };
                        messages.push(json!({
                            "role": "assistant",
                            "content": content,
                            "tool_calls": openai_tool_calls,
                        }));
                    } else {
                        messages.push(json!({
                            "role": "assistant",
                            "content": entry.content,
                        }));
                    }
                }
            }
        }

        messages
    }

    fn build_openai_tools_payload(
        &self,
        tools: &[ToolManifest],
    ) -> (Vec<Value>, HashMap<String, String>) {
        let mut openai_tools = Vec::new();
        let mut intent_by_tool = HashMap::new();
        let mut seen_names = HashSet::new();

        for manifest in tools {
            let tool_name = manifest.manifest.name.trim();
            if tool_name.is_empty() {
                continue;
            }
            if !seen_names.insert(tool_name.to_string()) {
                continue;
            }

            let intent_type = tool_helpers::infer_intent_type_from_permissions(
                &manifest.capabilities_required.permissions,
            );
            intent_by_tool.insert(tool_name.to_string(), intent_type);

            openai_tools.push(json!({
                "type": "function",
                "function": {
                    "name": tool_name,
                    "description": manifest.manifest.description,
                    "parameters": tool_helpers::normalize_tool_input_schema(manifest.input_schema.as_ref()),
                }
            }));
        }

        (openai_tools, intent_by_tool)
    }

    fn build_request_body(
        &self,
        messages: Vec<Value>,
        tools: &[ToolManifest],
    ) -> (Value, HashMap<String, String>) {
        let (openai_tools, intent_by_tool) = self.build_openai_tools_payload(tools);

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false
        });

        if !openai_tools.is_empty() {
            body["tools"] = Value::Array(openai_tools);
            body["tool_choice"] = json!("auto");
        }

        (body, intent_by_tool)
    }

    fn parse_message_content(message: &Value) -> String {
        match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }

    fn parse_tool_call_payload(tool_name: &str, arguments: Option<&Value>) -> Value {
        match arguments {
            Some(Value::String(raw)) => {
                if raw.trim().is_empty() {
                    json!({})
                } else {
                    match serde_json::from_str::<Value>(raw) {
                        Ok(value) => value,
                        Err(error) => {
                            warn!(
                                tool_name = tool_name,
                                error = %error,
                                "OpenAI tool call arguments were not valid JSON; using empty payload"
                            );
                            json!({})
                        }
                    }
                }
            }
            Some(Value::Object(_)) | Some(Value::Array(_)) => {
                arguments.cloned().unwrap_or_default()
            }
            Some(Value::Null) | None => json!({}),
            Some(_) => {
                warn!(
                    tool_name = tool_name,
                    "OpenAI tool call arguments were not an object/string; using empty payload"
                );
                json!({})
            }
        }
    }

    fn parse_openai_tool_calls(
        message: &Value,
        intent_by_tool: &HashMap<String, String>,
    ) -> Vec<InferenceToolCall> {
        let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
            return Vec::new();
        };

        let mut parsed = Vec::new();
        for call in calls {
            if call.get("type").and_then(Value::as_str) != Some("function") {
                continue;
            }

            let Some(function_obj) = call.get("function").and_then(Value::as_object) else {
                continue;
            };
            let Some(tool_name) = function_obj
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };

            let raw_payload =
                Self::parse_tool_call_payload(tool_name, function_obj.get("arguments"));
            let (payload, explicit_intent) = match raw_payload {
                Value::Object(mut obj) => {
                    // Extract explicit intent_type if the model included one.
                    // All remaining keys become the payload — we do NOT
                    // destructure a "payload" key because OpenAI function
                    // arguments are already the payload and a tool could
                    // legitimately use "payload" as an argument name.
                    let explicit_intent = obj
                        .remove("intent_type")
                        .and_then(|v| v.as_str().map(str::to_string));
                    (Value::Object(obj), explicit_intent)
                }
                other => (other, None),
            };

            if !tool_helpers::check_payload_size(tool_name, &payload) {
                continue;
            }

            let intent_type = explicit_intent.unwrap_or_else(|| {
                intent_by_tool
                    .get(tool_name)
                    .cloned()
                    .unwrap_or_else(|| "query".to_string())
            });

            parsed.push(InferenceToolCall {
                id: call.get("id").and_then(Value::as_str).map(str::to_string),
                tool_name: tool_name.to_string(),
                intent_type,
                payload,
            });
        }

        parsed
    }

    fn parse_response_json(
        &self,
        json_resp: &Value,
        intent_by_tool: &HashMap<String, String>,
        duration_ms: u64,
    ) -> Result<InferenceResult, AgentOSError> {
        let message = json_resp
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .ok_or_else(|| AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: "Missing choices[0].message in OpenAI response".to_string(),
            })?;

        let text = Self::parse_message_content(message);
        let tool_calls = Self::parse_openai_tool_calls(message, intent_by_tool);

        // Extract stop/finish reason from OpenAI response
        let finish_reason = json_resp["choices"][0]["finish_reason"]
            .as_str()
            .unwrap_or("stop");
        let stop_reason = match finish_reason {
            "stop" => StopReason::EndTurn,
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            "content_filter" => StopReason::ContentFilter,
            other => StopReason::Other(other.to_string()),
        };

        let prompt_tokens = json_resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        let completion_tokens = json_resp["usage"]["completion_tokens"]
            .as_u64()
            .unwrap_or(0);
        let total_tokens = json_resp["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let cached_tokens = json_resp["usage"]["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0);

        let tokens_used = TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        };
        let cost = calculate_inference_cost(&tokens_used, &self.pricing);

        Ok(InferenceResult {
            text,
            tokens_used,
            model: self.model.clone(),
            duration_ms,
            tool_calls,
            uncertainty: None,
            stop_reason,
            cost: Some(cost),
            cached_tokens,
        })
    }
}

#[async_trait]
impl LLMCore for OpenAICore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        self.infer_with_tools(context, &[]).await
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
        let estimated = self.estimate_tokens(context, tools);
        let max = self.capabilities.context_window_tokens;
        if estimated > max {
            return Err(AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        let url = format!("{}/chat/completions", self.base_url);
        let messages = self.format_messages(context);

        // If options disable tools, exclude them from the request body.
        let effective_tools = if matches!(options.tool_choice, Some(ToolChoice::None)) {
            &[][..]
        } else {
            tools
        };
        let (mut body, intent_by_tool) = self.build_request_body(messages, effective_tools);

        // Apply tool_choice override.
        match &options.tool_choice {
            Some(ToolChoice::Auto) => {
                body["tool_choice"] = json!("auto");
            }
            Some(ToolChoice::None) => {} // tools excluded above; key was never set
            Some(ToolChoice::Required) => {
                body["tool_choice"] = json!("required");
            }
            Some(ToolChoice::Specific(name)) => {
                body["tool_choice"] = json!({"type": "function", "function": {"name": name}});
            }
            None => {} // leave default ("auto" if tools present)
        }

        if options.json_mode {
            body["response_format"] = json!({"type": "json_object"});
        }
        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }
        if let Some(max_tok) = options.max_tokens {
            body["max_tokens"] = json!(max_tok);
        }
        if let Some(seed) = options.seed {
            body["seed"] = json!(seed);
        }

        let res = crate::retry::send_with_retry(
            "openai",
            &self.retry_policy,
            &self.circuit_breaker,
            || {
                self.client
                    .post(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", self.api_key.expose_secret()),
                    )
                    .header("Content-Type", "application/json")
                    .json(&body)
            },
        )
        .await?;

        let json_resp: serde_json::Value =
            res.json().await.map_err(|e| AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!("Failed to parse JSON response: {}", e),
            })?;
        self.parse_response_json(
            &json_resp,
            &intent_by_tool,
            start_time.elapsed().as_millis() as u64,
        )
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        use crate::types::HealthStatus;
        let start = std::time::Instant::now();
        let url = format!("{}/models", self.base_url);
        match self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => {
                let latency = start.elapsed();
                if latency > std::time::Duration::from_secs(2) {
                    HealthStatus::Degraded {
                        reason: format!("High latency: {}ms", latency.as_millis()),
                    }
                } else {
                    HealthStatus::Healthy
                }
            }
            Ok(res) => HealthStatus::Unhealthy {
                reason: format!("HTTP {}", res.status()),
            },
            Err(e) => HealthStatus::Unhealthy {
                reason: format!("Connection failed: {e}"),
            },
        }
    }

    fn provider_name(&self) -> &str {
        "openai"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    async fn infer_stream_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        let estimated = self.estimate_tokens(context, tools);
        let max = self.capabilities.context_window_tokens;
        if estimated > max {
            return Err(AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        let url = format!("{}/chat/completions", self.base_url);
        let messages = self.format_messages(context);
        let (openai_tools, intent_by_tool) = self.build_openai_tools_payload(tools);

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true }
        });
        if !openai_tools.is_empty() {
            body["tools"] = Value::Array(openai_tools);
            body["tool_choice"] = json!("auto");
        }

        let res = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!("Reqwest failed: {}", e),
            })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            let err_msg = format!("OpenAI API error {}: {}", status, text);
            let _ = tx.send(InferenceEvent::Error(err_msg.clone())).await;
            return Err(AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: err_msg,
            });
        }

        // State for accumulating the streamed response.
        let mut full_text = String::new();
        let mut partial_tool_calls: Vec<PartialToolCall> = Vec::new();
        let mut usage = TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        let mut cached_tokens: u64 = 0;
        let mut stop_reason = StopReason::EndTurn;
        let mut line_buffer = String::new();

        const MAX_LINE_BUFFER_BYTES: usize = 1_048_576; // 1 MB

        let mut stream = res.bytes_stream();
        'outer: while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&chunk_str);

            if line_buffer.len() > MAX_LINE_BUFFER_BYTES {
                let err_msg = "SSE line buffer exceeded 1 MB";
                let _ = tx.send(InferenceEvent::Error(err_msg.to_string())).await;
                return Err(AgentOSError::LLMError {
                    provider: "openai".to_string(),
                    reason: err_msg.to_string(),
                });
            }

            // Process complete SSE lines from the buffer.
            while let Some(newline_pos) = line_buffer.find('\n') {
                let line = line_buffer[..newline_pos].trim().to_string();
                line_buffer = line_buffer[newline_pos + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                let data = if let Some(d) = line.strip_prefix("data: ") {
                    d.trim()
                } else {
                    continue;
                };
                if data == "[DONE]" {
                    break 'outer;
                }
                let Ok(chunk_json) = serde_json::from_str::<Value>(data) else {
                    continue;
                };

                // Extract finish_reason if present.
                if let Some(reason) = chunk_json["choices"][0]["finish_reason"].as_str() {
                    stop_reason = match reason {
                        "stop" => StopReason::EndTurn,
                        "tool_calls" => StopReason::ToolUse,
                        "length" => StopReason::MaxTokens,
                        "content_filter" => StopReason::ContentFilter,
                        other => StopReason::Other(other.to_string()),
                    };
                }

                // Text delta.
                if let Some(content) = chunk_json["choices"][0]["delta"]["content"].as_str() {
                    if !content.is_empty() {
                        full_text.push_str(content);
                        let _ = tx.send(InferenceEvent::Token(content.to_string())).await;
                    }
                }

                // Tool call deltas.
                if let Some(tc_deltas) = chunk_json["choices"][0]["delta"]["tool_calls"].as_array()
                {
                    for tc_delta in tc_deltas {
                        let index = tc_delta["index"].as_u64().unwrap_or(0) as usize;

                        // Ensure we have a slot for this index.
                        while partial_tool_calls.len() <= index {
                            partial_tool_calls.push(PartialToolCall {
                                id: None,
                                name: String::new(),
                                arguments_buffer: String::new(),
                            });
                        }

                        let partial = &mut partial_tool_calls[index];

                        // First delta for this index carries id and function.name.
                        if let Some(id) = tc_delta["id"].as_str() {
                            partial.id = Some(id.to_string());
                        }
                        if let Some(name) = tc_delta["function"]["name"].as_str() {
                            partial.name = name.to_string();
                            let _ = tx
                                .send(InferenceEvent::ToolCallStart {
                                    index,
                                    id: partial.id.clone(),
                                    tool_name: name.to_string(),
                                })
                                .await;
                        }

                        // Argument chunks.
                        if let Some(args_chunk) = tc_delta["function"]["arguments"].as_str() {
                            partial.arguments_buffer.push_str(args_chunk);
                            let _ = tx
                                .send(InferenceEvent::ToolCallDelta {
                                    index,
                                    arguments_chunk: args_chunk.to_string(),
                                })
                                .await;
                        }
                    }
                }

                // Usage in final chunk.
                if let Some(usage_obj) = chunk_json.get("usage") {
                    if usage_obj.is_object() && !usage_obj.is_null() {
                        usage.prompt_tokens = usage_obj["prompt_tokens"].as_u64().unwrap_or(0);
                        usage.completion_tokens =
                            usage_obj["completion_tokens"].as_u64().unwrap_or(0);
                        usage.total_tokens = usage_obj["total_tokens"].as_u64().unwrap_or(0);
                        cached_tokens = usage_obj["prompt_tokens_details"]["cached_tokens"]
                            .as_u64()
                            .unwrap_or(0);
                        let _ = tx.send(InferenceEvent::Usage(usage.clone())).await;
                    }
                }
            }
        }

        // Assemble completed tool calls.
        let mut tool_calls = Vec::new();
        for partial in &partial_tool_calls {
            if partial.name.is_empty() {
                continue;
            }
            let payload = Self::parse_tool_call_payload(
                &partial.name,
                Some(&Value::String(partial.arguments_buffer.clone())),
            );
            let intent_type = intent_by_tool
                .get(&partial.name)
                .cloned()
                .unwrap_or_else(|| "query".to_string());

            let tc = InferenceToolCall {
                id: partial.id.clone(),
                tool_name: partial.name.clone(),
                intent_type,
                payload: tool_helpers::validate_payload_object(
                    &partial.name,
                    "openai",
                    Some(payload),
                ),
            };
            let _ = tx.send(InferenceEvent::ToolCallComplete(tc.clone())).await;
            tool_calls.push(tc);
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let cost = calculate_inference_cost(&usage, &self.pricing);

        let result = InferenceResult {
            text: full_text,
            tokens_used: usage,
            model: self.model.clone(),
            duration_ms,
            tool_calls,
            uncertainty: None,
            stop_reason,
            cost: Some(cost),
            cached_tokens,
        };
        let _ = tx.send(InferenceEvent::Done(result)).await;
        Ok(())
    }
}

/// Accumulator for a tool call being streamed in chunks (OpenAI).
struct PartialToolCall {
    id: Option<String>,
    name: String,
    arguments_buffer: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::tool::{ToolCapabilities, ToolExecutor, ToolInfo, ToolOutputs, ToolSchema};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn test_format_messages() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "You are a helpful assistant.".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "status: ok".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4".into());
        let messages = adapter.format_messages(&ctx);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"], "Tool Result:\nstatus: ok");
    }

    fn make_manifest(
        name: &str,
        description: &str,
        permissions: Vec<&str>,
        input_schema: Option<Value>,
    ) -> ToolManifest {
        ToolManifest {
            manifest: ToolInfo {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: description.to_string(),
                author: "agentos-core".to_string(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier: TrustTier::Core,
                tags: None,
            },
            capabilities_required: ToolCapabilities {
                permissions: permissions.into_iter().map(str::to_string).collect(),
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "Input".to_string(),
                output: "Output".to_string(),
            },
            input_schema,
            sandbox: ToolSandbox {
                network: false,
                fs_write: false,
                gpu: false,
                max_memory_mb: 64,
                max_cpu_ms: 1000,
                syscalls: vec![],
                weight: None,
            },
            executor: ToolExecutor::default(),
        }
    }

    fn find_header_terminator(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
    }

    fn parse_content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    fn read_http_body(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        let mut expected_total_len: Option<usize> = None;

        loop {
            let n = stream.read(&mut chunk).expect("failed to read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);

            if expected_total_len.is_none() {
                if let Some(headers_end) = find_header_terminator(&buf) {
                    let headers = String::from_utf8_lossy(&buf[..headers_end]);
                    let content_len = parse_content_length(&headers);
                    expected_total_len = Some(headers_end + content_len);
                }
            }

            if let Some(total_len) = expected_total_len {
                if buf.len() >= total_len {
                    break;
                }
            }
        }

        let headers_end = find_header_terminator(&buf).expect("missing HTTP header terminator");
        String::from_utf8_lossy(&buf[headers_end..]).into_owned()
    }

    #[test]
    fn test_build_request_body_includes_openai_tools() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let manifest = make_manifest(
            "file-reader",
            "Read a file",
            vec!["fs.user_data:r"],
            Some(json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string" }
                }
            })),
        );

        let messages = vec![json!({
            "role": "user",
            "content": "read /tmp/a.txt"
        })];

        let (body, intent_map) = adapter.build_request_body(messages, &[manifest]);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "file-reader");
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        assert_eq!(intent_map.get("file-reader"), Some(&"read".to_string()));
    }

    #[test]
    fn test_parse_response_extracts_tool_calls() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());

        let response = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "file-reader",
                            "arguments": "{\"path\":\"test.txt\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 3,
                "total_tokens": 15
            }
        });

        let result = adapter
            .parse_response_json(&response, &intent_map, 42)
            .expect("response should parse");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id.as_deref(), Some("call_abc"));
        assert_eq!(result.tool_calls[0].tool_name, "file-reader");
        assert_eq!(result.tool_calls[0].intent_type, "read");
        assert_eq!(result.tool_calls[0].payload["path"], "test.txt");
        assert_eq!(result.tokens_used.total_tokens, 15);
    }

    #[test]
    fn test_parse_response_preserves_reasoning_text_with_tool_calls() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());

        let response = json!({
            "choices": [{
                "message": {
                    "content": "I will read the file first.",
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "file-reader",
                            "arguments": "{\"path\":\"test.txt\"}"
                        }
                    }]
                }
            }],
            "usage": {}
        });

        let result = adapter
            .parse_response_json(&response, &intent_map, 9)
            .expect("response should parse");
        assert_eq!(result.text, "I will read the file first.");
        assert_eq!(result.tool_calls.len(), 1);
    }

    #[test]
    fn test_parse_response_with_text_only() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let response = json!({
            "choices": [{
                "message": {
                    "content": "Final answer"
                }
            }],
            "usage": {
                "prompt_tokens": 4,
                "completion_tokens": 2,
                "total_tokens": 6
            }
        });

        let result = adapter
            .parse_response_json(&response, &HashMap::new(), 7)
            .expect("response should parse");
        assert_eq!(result.text, "Final answer");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.tokens_used.total_tokens, 6);
    }

    #[tokio::test]
    async fn test_infer_with_tools_sends_openai_tools_and_parses_tool_calls() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel::<Value>();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let req_body = read_http_body(&mut stream);
            let req_json: Value = serde_json::from_str(&req_body).expect("valid JSON body");
            tx.send(req_json).expect("send request body to test");

            let response_body = json!({
                "choices": [{
                    "message": {
                        "content": "Thinking before calling tool.",
                        "tool_calls": [{
                            "id": "call_abc",
                            "type": "function",
                            "function": {
                                "name": "file-reader",
                                "arguments": "{\"path\":\"test.txt\"}"
                            }
                        }]
                    }
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15
                }
            })
            .to_string();

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let adapter = OpenAICore::with_base_url(
            SecretString::new("fake-key".into()),
            "gpt-4o".into(),
            format!("http://{}", addr),
        );

        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Read test.txt".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let manifest = make_manifest(
            "file-reader",
            "Read a file",
            vec!["fs.user_data:r"],
            Some(json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {"type": "string"}
                }
            })),
        );

        let result = adapter
            .infer_with_tools(&ctx, &[manifest])
            .await
            .expect("inference should succeed");

        let captured = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("captured request");
        assert_eq!(captured["tool_choice"], "auto");
        assert_eq!(captured["tools"][0]["type"], "function");
        assert_eq!(captured["tools"][0]["function"]["name"], "file-reader");
        assert_eq!(
            captured["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );

        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].tool_name, "file-reader");
        assert_eq!(result.tool_calls[0].intent_type, "read");
        assert_eq!(result.tool_calls[0].payload["path"], "test.txt");
        assert!(result.text.contains("Thinking before calling tool."));

        server.join().expect("server thread should complete");
    }

    #[test]
    fn test_stop_reason_tool_calls() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());
        let response = json!({
            "choices": [{"message": {"content": null, "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": {"name": "file-reader", "arguments": "{}"}
            }]}, "finish_reason": "tool_calls"}],
            "usage": {}
        });
        let result = adapter
            .parse_response_json(&response, &intent_map, 1)
            .unwrap();
        assert_eq!(result.stop_reason, crate::types::StopReason::ToolUse);
    }

    #[test]
    fn test_stop_reason_max_tokens() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let response = json!({
            "choices": [{"message": {"content": "truncated"}, "finish_reason": "length"}],
            "usage": {}
        });
        let result = adapter
            .parse_response_json(&response, &HashMap::new(), 1)
            .unwrap();
        assert_eq!(result.stop_reason, crate::types::StopReason::MaxTokens);
    }

    #[test]
    fn test_stop_reason_content_filter() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let response = json!({
            "choices": [{"message": {"content": ""}, "finish_reason": "content_filter"}],
            "usage": {}
        });
        let result = adapter
            .parse_response_json(&response, &HashMap::new(), 1)
            .unwrap();
        assert_eq!(result.stop_reason, crate::types::StopReason::ContentFilter);
    }

    #[test]
    fn test_stop_reason_end_turn() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let response = json!({
            "choices": [{"message": {"content": "done"}, "finish_reason": "stop"}],
            "usage": {}
        });
        let result = adapter
            .parse_response_json(&response, &HashMap::new(), 1)
            .unwrap();
        assert_eq!(result.stop_reason, crate::types::StopReason::EndTurn);
    }

    #[test]
    fn test_cached_tokens_extracted() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let response = json!({
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 10,
                "total_tokens": 110,
                "prompt_tokens_details": {"cached_tokens": 80}
            }
        });
        let result = adapter
            .parse_response_json(&response, &HashMap::new(), 1)
            .unwrap();
        assert_eq!(result.cached_tokens, 80);
    }

    #[test]
    fn test_format_messages_native_tool_result() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: r#"{"status": "ok"}"#.to_string(),
            metadata: Some(ContextMetadata {
                tool_name: Some("file-reader".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("call_abc123".to_string()),
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

        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4".into());
        let messages = adapter.format_messages(&ctx);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_abc123");
        assert_eq!(messages[0]["content"], r#"{"status": "ok"}"#);
    }

    #[test]
    fn test_format_messages_legacy_tool_result_without_metadata() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "status: ok".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4".into());
        let messages = adapter.format_messages(&ctx);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Tool Result:\nstatus: ok");
    }
}
