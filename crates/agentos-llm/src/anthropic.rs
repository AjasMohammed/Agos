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

/// Anthropic API adapter for Claude models.
pub struct AnthropicCore {
    client: Client,
    api_key: SecretString,
    model: String,
    base_url: String,
    /// Maximum tokens to generate per response. Configurable via `llm.max_tokens`.
    max_tokens: u32,
    capabilities: ModelCapabilities,
    pricing: ModelPricing,
    retry_policy: crate::retry::RetryPolicy,
    circuit_breaker: crate::retry::CircuitBreaker,
}

impl AnthropicCore {
    /// Default maximum output tokens used when no config value is provided.
    pub const DEFAULT_MAX_TOKENS: u32 = 8192;

    /// Create a new Anthropic adapter using the default API base URL.
    pub fn new(api_key: SecretString, model: String) -> Self {
        Self::with_base_url(api_key, model, "https://api.anthropic.com/v1".to_string())
    }

    /// Create a new Anthropic adapter with a custom base URL (e.g., for enterprise proxies or tests).
    pub fn with_base_url(api_key: SecretString, model: String, base_url: String) -> Self {
        let table = default_pricing_table();
        let pricing = table
            .iter()
            .find(|p| p.provider == "anthropic" && p.model == model)
            .or_else(|| {
                table
                    .iter()
                    .find(|p| p.provider == "anthropic" && p.model == "*")
            })
            .cloned()
            .unwrap_or(ModelPricing {
                provider: "anthropic".to_string(),
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
            max_tokens: Self::DEFAULT_MAX_TOKENS,
            capabilities: ModelCapabilities {
                context_window_tokens: 200_000,
                supports_images: true,
                supports_tool_calling: true,
                supports_json_mode: false,
                max_output_tokens: Self::DEFAULT_MAX_TOKENS as u64,
                supports_streaming: true,
                supports_parallel_tools: true,
                supports_prompt_caching: true,
                supports_thinking: true,
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

    /// Override the maximum output tokens for each request.
    ///
    /// Call this after construction to apply a value from kernel config
    /// (`llm.max_tokens`). Panics if `max_tokens` is zero.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        assert!(max_tokens > 0, "max_tokens must be greater than zero");
        self.max_tokens = max_tokens;
        self.capabilities.max_output_tokens = max_tokens as u64;
        self
    }

    fn format_messages(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut messages: Vec<serde_json::Value> = Vec::new();
        let mut pending_tool_results: Vec<serde_json::Value> = Vec::new();

        for entry in context.active_entries() {
            match entry.role {
                ContextRole::System => continue, // Anthropic wants system prompt top-level
                ContextRole::ToolResult => {
                    let tool_use_id = entry
                        .metadata
                        .as_ref()
                        .and_then(|m| m.tool_call_id.as_deref());

                    if let Some(use_id) = tool_use_id {
                        // Native Anthropic tool result content block.
                        pending_tool_results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": use_id,
                            "content": entry.content,
                        }));
                    } else {
                        // Legacy fallback: add as a text content block in the
                        // pending batch to avoid consecutive user messages.
                        pending_tool_results.push(json!({
                            "type": "text",
                            "text": format!("Tool Result:\n{}", entry.content),
                        }));
                    }
                }
                _ => {
                    // Flush pending tool results before any non-tool-result entry.
                    if !pending_tool_results.is_empty() {
                        messages.push(json!({
                            "role": "user",
                            "content": std::mem::take(&mut pending_tool_results),
                        }));
                    }
                    match entry.role {
                        ContextRole::User => {
                            messages.push(json!({
                                "role": "user",
                                "content": entry.content,
                            }));
                        }
                        ContextRole::Assistant => {
                            // If this assistant turn invoked tools, reconstruct the
                            // Anthropic-native format with tool_use content blocks.
                            // Anthropic requires tool_result blocks to reference a
                            // tool_use block that appeared in the preceding assistant turn.
                            if let Some(Value::Array(calls)) = entry
                                .metadata
                                .as_ref()
                                .and_then(|m| m.assistant_tool_calls.as_ref())
                            {
                                let mut content_blocks: Vec<Value> = Vec::new();
                                if !entry.content.is_empty() {
                                    content_blocks
                                        .push(json!({"type": "text", "text": entry.content}));
                                }
                                for (idx, call) in calls.iter().enumerate() {
                                    if let Some(name) =
                                        call.get("tool_name").and_then(|v| v.as_str())
                                    {
                                        // Use provider-native ID if available; fall back to a
                                        // unique positional ID so parallel tool calls each get
                                        // a distinct ID (Anthropic requires every tool_use ID
                                        // to be unique within a turn).
                                        let id = call
                                            .get("id")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                            .unwrap_or_else(|| format!("toolu_{idx}"));
                                        let input = call
                                            .get("payload")
                                            .cloned()
                                            .unwrap_or_else(|| json!({}));
                                        content_blocks.push(json!({
                                            "type": "tool_use",
                                            "id": id,
                                            "name": name,
                                            "input": input,
                                        }));
                                    }
                                }
                                messages.push(json!({
                                    "role": "assistant",
                                    "content": content_blocks,
                                }));
                            } else {
                                messages.push(json!({
                                    "role": "assistant",
                                    "content": entry.content,
                                }));
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
        // Flush remaining pending tool results.
        if !pending_tool_results.is_empty() {
            messages.push(json!({
                "role": "user",
                "content": pending_tool_results,
            }));
        }

        messages
    }

    fn build_anthropic_tools(tools: &[ToolManifest]) -> (Vec<Value>, HashMap<String, String>) {
        let mut anthropic_tools = Vec::new();
        let mut intent_by_tool = HashMap::new();
        let mut seen_names = HashSet::new();

        for manifest in tools {
            let tool_name = manifest.manifest.name.trim();
            if tool_name.is_empty() || !seen_names.insert(tool_name.to_string()) {
                continue;
            }

            let intent_type = tool_helpers::infer_intent_type_from_permissions(
                &manifest.capabilities_required.permissions,
            );
            intent_by_tool.insert(tool_name.to_string(), intent_type);

            anthropic_tools.push(json!({
                "name": tool_name,
                "description": manifest.manifest.description,
                "input_schema": tool_helpers::normalize_tool_input_schema(manifest.input_schema.as_ref()),
            }));
        }

        (anthropic_tools, intent_by_tool)
    }

    fn parse_anthropic_tool_calls(
        content_blocks: &[Value],
        intent_by_tool: &HashMap<String, String>,
    ) -> (String, Vec<InferenceToolCall>) {
        let mut text = String::new();
        let mut tool_calls = Vec::new();

        for block in content_blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let Some(tool_name) = block
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                    else {
                        continue;
                    };

                    let id = block.get("id").and_then(Value::as_str).map(str::to_string);
                    let payload = tool_helpers::validate_payload_object(
                        tool_name,
                        "anthropic",
                        block.get("input").cloned(),
                    );

                    if !tool_helpers::check_payload_size(tool_name, &payload) {
                        continue;
                    }

                    let intent_type = intent_by_tool
                        .get(tool_name)
                        .cloned()
                        .unwrap_or_else(|| "query".to_string());

                    tool_calls.push(InferenceToolCall {
                        id,
                        tool_name: tool_name.to_string(),
                        intent_type,
                        payload,
                    });
                }
                _ => {}
            }
        }

        (text, tool_calls)
    }
}

#[async_trait]
impl LLMCore for AnthropicCore {
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
                provider: "anthropic".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        let url = format!("{}/messages", self.base_url);

        let messages = self.format_messages(context);
        let active = context.active_entries();
        let system_prompt = active
            .iter()
            .find(|e| e.role == ContextRole::System)
            .map(|e| e.content.as_str())
            .unwrap_or("");

        let max_tokens = options.max_tokens.unwrap_or(self.max_tokens);

        // If options disable tools, exclude them from the request.
        let effective_tools = if matches!(options.tool_choice, Some(ToolChoice::None)) {
            &[][..]
        } else {
            tools
        };
        let (anthropic_tools, intent_by_tool) = Self::build_anthropic_tools(effective_tools);

        let mut body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "system": system_prompt,
            "messages": messages,
        });

        if !anthropic_tools.is_empty() {
            body["tools"] = Value::Array(anthropic_tools);
            // Apply tool_choice override (Anthropic format).
            match &options.tool_choice {
                Some(ToolChoice::Required) => {
                    body["tool_choice"] = json!({"type": "any"});
                }
                Some(ToolChoice::Specific(name)) => {
                    body["tool_choice"] = json!({"type": "tool", "name": name});
                }
                Some(ToolChoice::Auto) | None => {
                    body["tool_choice"] = json!({"type": "auto"});
                }
                Some(ToolChoice::None) => {} // tools excluded above
            }
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        let res = crate::retry::send_with_retry(
            "anthropic",
            &self.retry_policy,
            &self.circuit_breaker,
            || {
                self.client
                    .post(&url)
                    .header("x-api-key", self.api_key.expose_secret())
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .json(&body)
            },
        )
        .await?;

        let json_resp: serde_json::Value =
            res.json().await.map_err(|e| AgentOSError::LLMError {
                provider: "anthropic".to_string(),
                reason: format!("Failed to parse JSON response: {}", e),
            })?;

        let content_blocks = json_resp["content"].as_array().cloned().unwrap_or_default();
        let (text, tool_calls) = Self::parse_anthropic_tool_calls(&content_blocks, &intent_by_tool);

        let stop_reason_str = json_resp["stop_reason"].as_str().unwrap_or("end_turn");
        let stop_reason = match stop_reason_str {
            "end_turn" => StopReason::EndTurn,
            "tool_use" => StopReason::ToolUse,
            "max_tokens" => StopReason::MaxTokens,
            "stop_sequence" => StopReason::StopSequence,
            other => StopReason::Other(other.to_string()),
        };

        let prompt_tokens = json_resp["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let completion_tokens = json_resp["usage"]["output_tokens"].as_u64().unwrap_or(0);
        let total_tokens = prompt_tokens + completion_tokens;
        let cached_tokens = json_resp["usage"]["cache_read_input_tokens"]
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
            duration_ms: start_time.elapsed().as_millis() as u64,
            tool_calls,
            uncertainty: None,
            stop_reason,
            cost: Some(cost),
            cached_tokens,
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        use crate::types::HealthStatus;
        let start = std::time::Instant::now();
        let url = format!("{}/messages", self.base_url);
        let body = json!({
            "model": self.model,
            "max_tokens": 1,
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });

        match self
            .client
            .post(&url)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => {
                let latency = start.elapsed();
                if latency > std::time::Duration::from_secs(3) {
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
        "anthropic"
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
                provider: "anthropic".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        let url = format!("{}/messages", self.base_url);

        let messages = self.format_messages(context);
        let active = context.active_entries();
        let system_prompt = active
            .iter()
            .find(|e| e.role == ContextRole::System)
            .map(|e| e.content.as_str())
            .unwrap_or("");

        let (anthropic_tools, intent_by_tool) = Self::build_anthropic_tools(tools);

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system_prompt,
            "messages": messages,
            "stream": true,
        });
        if !anthropic_tools.is_empty() {
            body["tools"] = Value::Array(anthropic_tools);
            body["tool_choice"] = json!({"type": "auto"});
        }

        let res = self
            .client
            .post(&url)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "anthropic".to_string(),
                reason: format!("Reqwest failed: {}", e),
            })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            let err_msg = format!("Anthropic API error {}: {}", status, text);
            let _ = tx.send(InferenceEvent::Error(err_msg.clone())).await;
            return Err(AgentOSError::LLMError {
                provider: "anthropic".to_string(),
                reason: err_msg,
            });
        }

        // Streaming state.
        let mut full_text = String::new();
        let mut tool_calls: Vec<InferenceToolCall> = Vec::new();
        let mut usage = TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        let mut cached_tokens: u64 = 0;
        let mut stop_reason = StopReason::EndTurn;

        // Content block tracking.
        let mut current_block_type: Option<String> = None; // "text" or "tool_use"
        let mut current_tool_id: Option<String> = None;
        let mut current_tool_name: Option<String> = None;
        let mut current_tool_args_buffer = String::new();
        let mut tool_block_index: usize = 0;

        let mut line_buffer = String::new();
        let mut current_event_type = String::new();

        const MAX_LINE_BUFFER_BYTES: usize = 1_048_576; // 1 MB

        let mut stream = res.bytes_stream();
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "anthropic".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&chunk_str);

            if line_buffer.len() > MAX_LINE_BUFFER_BYTES {
                let err_msg = "SSE line buffer exceeded 1 MB";
                let _ = tx.send(InferenceEvent::Error(err_msg.to_string())).await;
                return Err(AgentOSError::LLMError {
                    provider: "anthropic".to_string(),
                    reason: err_msg.to_string(),
                });
            }

            while let Some(newline_pos) = line_buffer.find('\n') {
                let line = line_buffer[..newline_pos].trim().to_string();
                line_buffer = line_buffer[newline_pos + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                // Anthropic SSE format: "event: <type>" then "data: <json>"
                if let Some(event_type) = line.strip_prefix("event: ") {
                    current_event_type = event_type.trim().to_string();
                    continue;
                }

                let data = if let Some(d) = line.strip_prefix("data: ") {
                    d.trim()
                } else {
                    continue;
                };
                let Ok(data_json) = serde_json::from_str::<Value>(data) else {
                    continue;
                };

                match current_event_type.as_str() {
                    "message_start" => {
                        // Extract input token usage.
                        if let Some(u) = data_json["message"]["usage"].as_object() {
                            usage.prompt_tokens =
                                u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                            cached_tokens = u
                                .get("cache_read_input_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                        }
                    }
                    "content_block_start" => {
                        let block = &data_json["content_block"];
                        let block_type = block["type"].as_str().unwrap_or("text").to_string();

                        if block_type == "tool_use" {
                            current_tool_id = block["id"].as_str().map(str::to_string);
                            current_tool_name = block["name"].as_str().map(str::to_string);
                            current_tool_args_buffer.clear();

                            if let Some(ref name) = current_tool_name {
                                let _ = tx
                                    .send(InferenceEvent::ToolCallStart {
                                        index: tool_block_index,
                                        id: current_tool_id.clone(),
                                        tool_name: name.clone(),
                                    })
                                    .await;
                            }
                        }
                        current_block_type = Some(block_type);
                    }
                    "content_block_delta" => {
                        let delta = &data_json["delta"];
                        let delta_type = delta["type"].as_str().unwrap_or("");

                        if delta_type == "text_delta" {
                            if let Some(text) = delta["text"].as_str() {
                                full_text.push_str(text);
                                let _ = tx.send(InferenceEvent::Token(text.to_string())).await;
                            }
                        } else if delta_type == "input_json_delta" {
                            if let Some(partial) = delta["partial_json"].as_str() {
                                current_tool_args_buffer.push_str(partial);
                                let _ = tx
                                    .send(InferenceEvent::ToolCallDelta {
                                        index: tool_block_index,
                                        arguments_chunk: partial.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                    "content_block_stop" => {
                        if current_block_type.as_deref() == Some("tool_use") {
                            let tool_name = current_tool_name.take().unwrap_or_default();
                            let payload: Value = serde_json::from_str(&current_tool_args_buffer)
                                .unwrap_or_else(|_| json!({}));
                            let intent_type = intent_by_tool
                                .get(&tool_name)
                                .cloned()
                                .unwrap_or_else(|| "query".to_string());

                            let tc = InferenceToolCall {
                                id: current_tool_id.take(),
                                tool_name: tool_name.clone(),
                                intent_type,
                                payload: tool_helpers::validate_payload_object(
                                    &tool_name,
                                    "anthropic",
                                    Some(payload),
                                ),
                            };
                            let _ = tx.send(InferenceEvent::ToolCallComplete(tc.clone())).await;
                            tool_calls.push(tc);
                            tool_block_index += 1;
                            current_tool_args_buffer.clear();
                        }
                        current_block_type = None;
                    }
                    "message_delta" => {
                        if let Some(reason) = data_json["delta"]["stop_reason"].as_str() {
                            stop_reason = match reason {
                                "end_turn" => StopReason::EndTurn,
                                "tool_use" => StopReason::ToolUse,
                                "max_tokens" => StopReason::MaxTokens,
                                "stop_sequence" => StopReason::StopSequence,
                                other => StopReason::Other(other.to_string()),
                            };
                        }
                        if let Some(output) = data_json["usage"]["output_tokens"].as_u64() {
                            usage.completion_tokens = output;
                            usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
                            let _ = tx.send(InferenceEvent::Usage(usage.clone())).await;
                        }
                    }
                    "message_stop" => {
                        // Stream complete.
                    }
                    "error" => {
                        let err_msg = data_json["error"]["message"]
                            .as_str()
                            .unwrap_or("Unknown Anthropic stream error");
                        let _ = tx.send(InferenceEvent::Error(err_msg.to_string())).await;
                    }
                    _ => {}
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_messages_anthropic() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "System rules here.".to_string(),
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

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx);

        // System prompt is separated in Anthropic
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
    }

    #[test]
    fn test_parse_anthropic_tool_calls_extracts_tool_use() {
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());

        let content = vec![
            json!({"type": "text", "text": "I will read the file."}),
            json!({
                "type": "tool_use",
                "id": "toolu_abc123",
                "name": "file-reader",
                "input": {"path": "test.txt"}
            }),
        ];

        let (text, tool_calls) = AnthropicCore::parse_anthropic_tool_calls(&content, &intent_map);
        assert_eq!(text, "I will read the file.");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id.as_deref(), Some("toolu_abc123"));
        assert_eq!(tool_calls[0].tool_name, "file-reader");
        assert_eq!(tool_calls[0].intent_type, "read");
        assert_eq!(tool_calls[0].payload["path"], "test.txt");
    }

    #[test]
    fn test_parse_anthropic_tool_calls_text_only() {
        let content = vec![json!({"type": "text", "text": "Final answer."})];

        let (text, tool_calls) =
            AnthropicCore::parse_anthropic_tool_calls(&content, &HashMap::new());
        assert_eq!(text, "Final answer.");
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_default_max_tokens() {
        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        assert_eq!(adapter.max_tokens, AnthropicCore::DEFAULT_MAX_TOKENS);
        assert_eq!(
            adapter.capabilities().max_output_tokens,
            AnthropicCore::DEFAULT_MAX_TOKENS as u64
        );
    }

    #[test]
    fn test_with_max_tokens_updates_field_and_capabilities() {
        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into())
            .with_max_tokens(16384);
        assert_eq!(adapter.max_tokens, 16384);
        assert_eq!(adapter.capabilities().max_output_tokens, 16384);
    }

    #[test]
    #[should_panic(expected = "max_tokens must be greater than zero")]
    fn test_with_max_tokens_rejects_zero() {
        let _ = AnthropicCore::new(SecretString::new("fake".into()), "claude".into())
            .with_max_tokens(0);
    }

    #[test]
    fn test_build_anthropic_tools_deduplicates() {
        use agentos_types::tool::{
            ToolCapabilities, ToolExecutor, ToolInfo, ToolOutputs, ToolSchema,
        };
        let manifest = ToolManifest {
            manifest: ToolInfo {
                name: "file-reader".to_string(),
                version: "1.0.0".to_string(),
                description: "Read a file".to_string(),
                author: "core".to_string(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier: TrustTier::Core,
                tags: None,
            },
            capabilities_required: ToolCapabilities {
                permissions: vec!["fs.user_data:r".to_string()],
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "Input".to_string(),
                output: "Output".to_string(),
            },
            input_schema: Some(
                json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            ),
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
        };

        let (tools, intent_map) =
            AnthropicCore::build_anthropic_tools(&[manifest.clone(), manifest]);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "file-reader");
        assert_eq!(intent_map.get("file-reader"), Some(&"read".to_string()));
    }

    #[test]
    fn test_format_messages_native_tool_result() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Read the file".to_string(),
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
            content: "file contents here".to_string(),
            metadata: Some(ContextMetadata {
                tool_name: Some("file-reader".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("toolu_abc123".to_string()),
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

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx);

        assert_eq!(messages.len(), 2);
        // First message is the user message
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Read the file");
        // Second is the tool result batch (user role with content blocks)
        assert_eq!(messages[1]["role"], "user");
        let content = messages[1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_abc123");
        assert_eq!(content[0]["content"], "file contents here");
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

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx);

        // Legacy results are now emitted as text content blocks in a user message
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Tool Result:\nstatus: ok");
    }

    #[test]
    fn test_format_messages_consecutive_native_tool_results_batched() {
        let mut ctx = ContextWindow::new(5);
        // Two consecutive native tool results should be batched into one user message
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "result A".to_string(),
            metadata: Some(ContextMetadata {
                tool_name: Some("tool-a".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("toolu_a".to_string()),
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
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "result B".to_string(),
            metadata: Some(ContextMetadata {
                tool_name: Some("tool-b".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("toolu_b".to_string()),
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

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx);

        // Both tool results should be in a single user message
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["tool_use_id"], "toolu_a");
        assert_eq!(content[1]["tool_use_id"], "toolu_b");
    }

    #[test]
    fn test_format_messages_mixed_native_and_legacy_no_consecutive_user() {
        let mut ctx = ContextWindow::new(5);
        // Native tool result followed by legacy tool result — must NOT produce
        // consecutive user messages (Anthropic rejects that).
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "native result".to_string(),
            metadata: Some(ContextMetadata {
                tool_name: Some("tool-a".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("toolu_a".to_string()),
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
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "legacy result".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx);

        // Both should be in a single user message (no consecutive user messages)
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "Tool Result:\nlegacy result");
    }

    /// Test the stop reason mapping used in infer_with_tools.
    #[test]
    fn test_anthropic_stop_reason_mapping() {
        let cases = vec![
            ("end_turn", StopReason::EndTurn),
            ("tool_use", StopReason::ToolUse),
            ("max_tokens", StopReason::MaxTokens),
            ("stop_sequence", StopReason::StopSequence),
            ("unknown_value", StopReason::Other("unknown_value".into())),
        ];
        for (input, expected) in cases {
            let result = match input {
                "end_turn" => StopReason::EndTurn,
                "tool_use" => StopReason::ToolUse,
                "max_tokens" => StopReason::MaxTokens,
                "stop_sequence" => StopReason::StopSequence,
                other => StopReason::Other(other.to_string()),
            };
            assert_eq!(result, expected, "Failed for input: {input}");
        }
    }
}
