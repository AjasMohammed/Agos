use crate::media::{openai_user_content_value, ImageResolver, NoopImageResolver};
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
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Custom OpenAI-compatible API adapter.
///
/// Powers all 20+ providers in `config/providers.toml` (DeepSeek, Groq,
/// Fireworks, Mistral, xAI, Cohere, etc.) via the standard OpenAI
/// `/chat/completions` endpoint. Supports tool calling and SSE streaming.
pub struct CustomCore {
    client: Client,
    api_key: Option<SecretString>,
    model: String,
    base_url: String,
    capabilities: ModelCapabilities,
    pricing: ModelPricing,
    retry_policy: crate::retry::RetryPolicy,
    circuit_breaker: crate::retry::CircuitBreaker,
    image_resolver: Arc<dyn ImageResolver>,
    /// When non-empty, only these model names receive native image payloads.
    vision_models: Vec<String>,
}

impl CustomCore {
    /// Create a new Custom adapter.
    pub fn new(api_key: Option<SecretString>, model: String, base_url: String) -> Self {
        let table = default_pricing_table();
        let pricing = table
            .iter()
            .find(|p| p.provider == "custom" && p.model == model)
            .or_else(|| {
                table
                    .iter()
                    .find(|p| p.provider == "custom" && p.model == "*")
            })
            .cloned()
            .unwrap_or(ModelPricing {
                provider: "custom".to_string(),
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
                context_window_tokens: 32768,
                supports_images: false,
                supports_tool_calling: true,
                supports_json_mode: false,
                max_output_tokens: 0,
                supports_streaming: true,
                supports_parallel_tools: true,
                supports_prompt_caching: false,
                supports_thinking: false,
                supports_structured_output: false,
            },
            pricing,
            retry_policy: crate::retry::RetryPolicy::default(),
            circuit_breaker: crate::retry::CircuitBreaker::default(),
            image_resolver: Arc::new(NoopImageResolver),
            vision_models: Vec::new(),
        }
    }

    pub fn with_image_resolver(mut self, resolver: Arc<dyn ImageResolver>) -> Self {
        self.image_resolver = resolver;
        self
    }

    /// Restrict vision to specific model IDs from the provider catalog (`vision_models`).
    pub fn with_vision_models(mut self, models: Vec<String>) -> Self {
        self.vision_models = models;
        self
    }

    fn model_has_vision_in_catalog(&self) -> bool {
        if self.vision_models.is_empty() {
            return false;
        }
        let cur = self.model.trim();
        let cur_lc = cur.to_ascii_lowercase();
        self.vision_models.iter().any(|vm| {
            let v = vm.trim();
            if v.eq_ignore_ascii_case("auto") && cur_lc == "auto" {
                return true;
            }
            v == cur || v.eq_ignore_ascii_case(&cur_lc)
        })
    }

    /// Override the pricing for this adapter instance.
    pub fn with_pricing(mut self, pricing: ModelPricing) -> Self {
        self.pricing = pricing;
        self
    }

    /// Convert our internal `ContextWindow` to OpenAI-compatible messages array.
    fn format_messages(&self, context: &ContextWindow) -> Vec<Value> {
        let mut messages = Vec::new();

        for entry in context.active_entries() {
            match entry.role {
                ContextRole::ToolResult => {
                    let tool_call_id = entry
                        .metadata
                        .as_ref()
                        .and_then(|m| m.tool_call_id.as_deref());

                    if let Some(call_id) = tool_call_id {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": entry.text(),
                        }));
                    } else {
                        messages.push(json!({
                            "role": "user",
                            "content": format!("Tool Result:\n{}", entry.text()),
                        }));
                    }
                }
                ContextRole::System => {
                    messages.push(json!({
                        "role": "system",
                        "content": entry.text(),
                    }));
                }
                ContextRole::User => {
                    let content = openai_user_content_value(
                        entry,
                        self.supports_images(),
                        &self.image_resolver,
                    );
                    messages.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                ContextRole::Assistant => {
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
                        let content = if entry.text().is_empty() {
                            Value::Null
                        } else {
                            Value::String(entry.text().clone())
                        };
                        messages.push(json!({
                            "role": "assistant",
                            "content": content,
                            "tool_calls": openai_tool_calls,
                        }));
                    } else {
                        messages.push(json!({
                            "role": "assistant",
                            "content": entry.text(),
                        }));
                    }
                }
            }
        }

        messages
    }

    /// Build OpenAI-compatible tool definitions and a name→intent_type map.
    fn build_tools_payload(&self, tools: &[ToolManifest]) -> (Vec<Value>, HashMap<String, String>) {
        let mut openai_tools = Vec::new();
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

    /// Parse tool_calls from an OpenAI-compatible message object.
    fn parse_tool_calls(
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
                .filter(|n| !n.is_empty())
            else {
                continue;
            };

            let id = call.get("id").and_then(Value::as_str).map(str::to_string);
            let payload = Self::parse_tool_arguments(tool_name, function_obj.get("arguments"));
            let intent_type = intent_by_tool
                .get(tool_name)
                .cloned()
                .unwrap_or_else(|| "query".to_string());

            let payload = tool_helpers::validate_payload_object(tool_name, "custom", Some(payload));
            if !tool_helpers::check_payload_size(tool_name, &payload) {
                continue;
            }

            parsed.push(InferenceToolCall {
                id,
                tool_name: tool_name.to_string(),
                intent_type,
                payload,
            });
        }

        parsed
    }

    /// Fallback for small/local models that emit tool calls as JSON inside
    /// ```json fenced markdown blocks instead of structured `tool_calls`.
    /// Looks for objects shaped `{"tool": "<name>", "payload": {...}, ...}`
    /// (or `arguments`/`input`/`parameters` synonyms) and synthesizes
    /// `InferenceToolCall`s. Only matches names present in `intent_by_tool`
    /// to avoid promoting examples or hallucinated tools.
    fn parse_tool_calls_from_text(
        text: &str,
        intent_by_tool: &HashMap<String, String>,
    ) -> Vec<InferenceToolCall> {
        if text.is_empty() || intent_by_tool.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut search_from = 0;
        while let Some(rel) = text[search_from..].find("```") {
            let fence_open = search_from + rel;
            let after_open = fence_open + 3;
            let line_end = text[after_open..]
                .find('\n')
                .map(|n| after_open + n + 1)
                .unwrap_or(text.len());
            let lang = text[after_open..line_end].trim().to_ascii_lowercase();
            let body_start = line_end;
            let Some(close_rel) = text[body_start..].find("```") else {
                break;
            };
            let body_end = body_start + close_rel;
            search_from = body_end + 3;

            if !lang.is_empty() && lang != "json" {
                continue;
            }
            let body = text[body_start..body_end].trim();
            if body.is_empty() {
                continue;
            }

            let candidates: Vec<Value> = match serde_json::from_str::<Value>(body) {
                Ok(Value::Array(arr)) => arr,
                Ok(v) => vec![v],
                Err(_) => continue,
            };

            for cand in candidates {
                let Value::Object(obj) = cand else { continue };
                let Some(tool_name) = obj
                    .get("tool")
                    .or_else(|| obj.get("name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                else {
                    continue;
                };
                if !intent_by_tool.contains_key(tool_name) {
                    continue;
                }
                let payload = obj
                    .get("payload")
                    .or_else(|| obj.get("arguments"))
                    .or_else(|| obj.get("input"))
                    .or_else(|| obj.get("parameters"))
                    .cloned()
                    .unwrap_or(json!({}));
                let payload =
                    tool_helpers::validate_payload_object(tool_name, "custom", Some(payload));
                if !tool_helpers::check_payload_size(tool_name, &payload) {
                    continue;
                }
                let intent_type = obj
                    .get("intent_type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| intent_by_tool.get(tool_name).cloned())
                    .unwrap_or_else(|| "query".to_string());

                tracing::warn!(
                    tool = tool_name,
                    "Recovered tool call from fenced JSON in model text \
                     (small-model fallback). Provider should emit native tool_calls."
                );
                out.push(InferenceToolCall {
                    id: None,
                    tool_name: tool_name.to_string(),
                    intent_type,
                    payload,
                });
            }
        }
        out
    }

    /// Parse tool call arguments from OpenAI-compatible format.
    fn parse_tool_arguments(tool_name: &str, arguments: Option<&Value>) -> Value {
        match arguments {
            Some(Value::String(raw)) => {
                if raw.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str::<Value>(raw).unwrap_or_else(|e| {
                        tracing::warn!(
                            tool_name = tool_name,
                            error = %e,
                            "Custom tool call arguments were not valid JSON; using empty payload"
                        );
                        json!({})
                    })
                }
            }
            Some(Value::Object(_)) | Some(Value::Array(_)) => {
                arguments.cloned().unwrap_or_default()
            }
            _ => json!({}),
        }
    }

    /// Attach auth header if an API key is configured.
    fn auth_header(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key {
            req.header("Authorization", format!("Bearer {}", key.expose_secret()))
        } else {
            req
        }
    }
}

#[async_trait]
impl LLMCore for CustomCore {
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
                provider: "custom".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        let url = format!("{}/chat/completions", self.base_url);
        let prepared = crate::media::prepare_for_inference(
            context,
            crate::traits::LLMCore::supports_images(self),
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
        let messages = self.format_messages(context);

        let effective_tools = if matches!(options.tool_choice, Some(ToolChoice::None)) {
            &[][..]
        } else {
            tools
        };
        let (openai_tools, intent_by_tool) = self.build_tools_payload(effective_tools);

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false
        });

        if !openai_tools.is_empty() {
            body["tools"] = Value::Array(openai_tools);
            body["tool_choice"] = json!("auto");
        }

        // Apply options.
        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }
        if let Some(max_tok) = options.max_tokens {
            body["max_tokens"] = json!(max_tok);
        }

        let res = crate::retry::send_with_retry(
            "custom",
            &self.retry_policy,
            &self.circuit_breaker,
            || {
                self.auth_header(
                    self.client
                        .post(&url)
                        .header("Content-Type", "application/json")
                        .json(&body),
                )
            },
        )
        .await?;

        let json_resp: Value = res.json().await.map_err(|e| AgentOSError::LLMError {
            provider: "custom".to_string(),
            reason: format!("Failed to parse JSON response: {}", e),
        })?;

        let message = json_resp
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .ok_or_else(|| AgentOSError::LLMError {
                provider: "custom".to_string(),
                reason: "Missing choices[0].message in response".to_string(),
            })?;

        let text = match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let mut tool_calls = Self::parse_tool_calls(message, &intent_by_tool);

        // Small-model fallback: some local models (e.g. gemma) emit tool
        // calls as fenced JSON in `content` rather than structured
        // `tool_calls`. Recover them so the kernel doesn't coherence-reject.
        if tool_calls.is_empty() && !text.is_empty() {
            let recovered = Self::parse_tool_calls_from_text(&text, &intent_by_tool);
            if !recovered.is_empty() {
                tool_calls = recovered;
            }
        }

        // Strip tool-call JSON fences from text so the stored assistant turn
        // doesn't contain raw JSON that causes the model to loop on it.
        let text = tool_helpers::strip_tool_json_fences(&text, tool_calls.len());

        // Fallback to reasoning_content when content is empty and no tool calls.
        let text = if text.trim().is_empty() && tool_calls.is_empty() {
            message
                .get("reasoning_content")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(|s| {
                    tracing::info!(
                        model = %self.model,
                        reasoning_len = s.len(),
                        "Custom content empty, using reasoning_content as fallback"
                    );
                    s.to_string()
                })
                .unwrap_or(text)
        } else {
            text
        };

        let finish_reason = json_resp["choices"][0]["finish_reason"]
            .as_str()
            .unwrap_or("stop");
        let stop_reason = match finish_reason {
            "stop" if !tool_calls.is_empty() => StopReason::ToolUse,
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

    fn supports_images(&self) -> bool {
        self.model_has_vision_in_catalog()
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        use crate::types::HealthStatus;
        let start = std::time::Instant::now();
        let url = format!("{}/models", self.base_url);
        match self.auth_header(self.client.get(&url)).send().await {
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
        "custom"
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
                provider: "custom".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        let url = format!("{}/chat/completions", self.base_url);
        let prepared = crate::media::prepare_for_inference(
            context,
            crate::traits::LLMCore::supports_images(self),
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
        let messages = self.format_messages(context);
        let (openai_tools, intent_by_tool) = self.build_tools_payload(tools);

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
            .auth_header(
                self.client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .json(&body),
            )
            .send()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "custom".to_string(),
                reason: format!("Reqwest failed: {}", e),
            })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            let err_msg = format!("Custom API error {}: {}", status, text);
            let _ = tx.send(InferenceEvent::Error(err_msg.clone())).await;
            return Err(AgentOSError::LLMError {
                provider: "custom".to_string(),
                reason: err_msg,
            });
        }

        let mut full_text = String::new();
        let mut reasoning_text = String::new();
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
                provider: "custom".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&chunk_str);

            if line_buffer.len() > MAX_LINE_BUFFER_BYTES {
                let err_msg = "SSE line buffer exceeded 1 MB";
                let _ = tx.send(InferenceEvent::Error(err_msg.to_string())).await;
                return Err(AgentOSError::LLMError {
                    provider: "custom".to_string(),
                    reason: err_msg.to_string(),
                });
            }

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

                // Finish reason.
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

                // Reasoning content from reasoning models.
                if let Some(reasoning) =
                    chunk_json["choices"][0]["delta"]["reasoning_content"].as_str()
                {
                    if !reasoning.is_empty() {
                        reasoning_text.push_str(reasoning);
                    }
                }

                // Tool call deltas — accumulated incrementally.
                if let Some(tc_deltas) = chunk_json["choices"][0]["delta"]["tool_calls"].as_array()
                {
                    for tc_delta in tc_deltas {
                        let index = tc_delta["index"].as_u64().unwrap_or(0) as usize;

                        while partial_tool_calls.len() <= index {
                            partial_tool_calls.push(PartialToolCall {
                                id: None,
                                name: String::new(),
                                arguments_buffer: String::new(),
                            });
                        }

                        let partial = &mut partial_tool_calls[index];

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
            let payload = Self::parse_tool_arguments(
                &partial.name,
                Some(&Value::String(partial.arguments_buffer.clone())),
            );
            let intent_type = intent_by_tool
                .get(&partial.name)
                .cloned()
                .unwrap_or_else(|| "query".to_string());

            let payload =
                tool_helpers::validate_payload_object(&partial.name, "custom", Some(payload));
            if !tool_helpers::check_payload_size(&partial.name, &payload) {
                continue;
            }

            let tc = InferenceToolCall {
                id: partial.id.clone(),
                tool_name: partial.name.clone(),
                intent_type,
                payload,
            };
            let _ = tx.send(InferenceEvent::ToolCallComplete(tc.clone())).await;
            tool_calls.push(tc);
        }

        // Reasoning fallback for empty content.
        if full_text.trim().is_empty() && !reasoning_text.trim().is_empty() && tool_calls.is_empty()
        {
            tracing::info!(
                model = %self.model,
                reasoning_len = reasoning_text.len(),
                "Custom content empty but reasoning_content present — using as fallback"
            );
            full_text = reasoning_text;
        }

        // Small-model fallback: recover tool calls embedded as fenced JSON in
        // the streamed text when no native tool_calls deltas arrived.
        if tool_calls.is_empty() && !full_text.is_empty() {
            let recovered = Self::parse_tool_calls_from_text(&full_text, &intent_by_tool);
            for tc in &recovered {
                let _ = tx.send(InferenceEvent::ToolCallComplete(tc.clone())).await;
            }
            if !recovered.is_empty() {
                stop_reason = StopReason::ToolUse;
                full_text = tool_helpers::strip_tool_json_fences(&full_text, recovered.len());
                tool_calls = recovered;
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

/// Accumulator for a tool call being streamed in chunks.
struct PartialToolCall {
    id: Option<String>,
    name: String,
    arguments_buffer: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_calls_extracts_function_calls() {
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());

        let message = json!({
            "content": "",
            "tool_calls": [{
                "id": "call_abc",
                "type": "function",
                "function": {
                    "name": "file-reader",
                    "arguments": "{\"path\":\"test.txt\"}"
                }
            }]
        });

        let calls = CustomCore::parse_tool_calls(&message, &intent_map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "file-reader");
        assert_eq!(calls[0].intent_type, "read");
        assert_eq!(calls[0].id.as_deref(), Some("call_abc"));
        assert_eq!(calls[0].payload["path"], "test.txt");
    }

    #[test]
    fn test_parse_tool_calls_text_only_response() {
        let message = json!({
            "content": "Hello, world!"
        });
        let calls = CustomCore::parse_tool_calls(&message, &HashMap::new());
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_tool_arguments_string_json() {
        let payload =
            CustomCore::parse_tool_arguments("test-tool", Some(&json!("{\"key\":\"val\"}")));
        assert_eq!(payload["key"], "val");
    }

    #[test]
    fn test_parse_tool_arguments_invalid_json_returns_empty() {
        let payload = CustomCore::parse_tool_arguments("test-tool", Some(&json!("not valid json")));
        assert_eq!(payload, json!({}));
    }

    #[test]
    fn test_format_messages_native_tool_result() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: "result data".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: Some(ContextMetadata {
                tool_name: Some("file-reader".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("call_abc".to_string()),
                assistant_tool_calls: None,
            }),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });

        let adapter = CustomCore::new(
            None,
            "test-model".to_string(),
            "http://localhost".to_string(),
        );
        let messages = adapter.format_messages(&ctx);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_abc");
        assert_eq!(messages[0]["content"], "result data");
    }

    #[test]
    fn test_format_messages_legacy_tool_result() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: "result data".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });

        let adapter = CustomCore::new(
            None,
            "test-model".to_string(),
            "http://localhost".to_string(),
        );
        let messages = adapter.format_messages(&ctx);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Tool Result:\nresult data");
    }

    #[test]
    fn test_format_messages_user_image_url_with_vision_model() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![
                ContentPart::Text {
                    text: "describe".into(),
                },
                ContentPart::Image {
                    mime: "image/png".into(),
                    source: ImageSource::Base64 { data: "abc".into() },
                },
            ],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });

        let adapter = CustomCore::new(
            None,
            "pixtral-large-latest".into(),
            "http://localhost".into(),
        )
        .with_vision_models(vec!["pixtral-large-latest".into()]);
        let messages = adapter.format_messages(&ctx);
        assert_eq!(messages[0]["role"], "user");
        let content = &messages[0]["content"];
        assert!(content.is_array());
        let arr = content.as_array().unwrap();
        assert!(arr.iter().any(|v| v["type"] == "image_url"));
    }

    #[test]
    fn test_format_messages_user_image_text_only_when_model_not_in_vision_list() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![
                ContentPart::Text {
                    text: "describe".into(),
                },
                ContentPart::Image {
                    mime: "image/png".into(),
                    source: ImageSource::Base64 {
                        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                            .into(),
                    },
                },
            ],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });

        let adapter = CustomCore::new(None, "deepseek-chat".into(), "http://localhost".into())
            .with_vision_models(vec!["pixtral-large-latest".into()]);
        let messages = adapter.format_messages(&ctx);
        let content = &messages[0]["content"];
        let blob = content.to_string();
        assert!(
            blob.contains("model does not support vision") || blob.contains("[Image:"),
            "{blob}"
        );
    }

    #[test]
    fn test_capabilities_reflect_tool_and_streaming_support() {
        let adapter = CustomCore::new(None, "test".to_string(), "http://localhost".to_string());
        let caps = adapter.capabilities();
        assert!(caps.supports_tool_calling);
        assert!(caps.supports_streaming);
        assert!(caps.supports_parallel_tools);
    }
}
