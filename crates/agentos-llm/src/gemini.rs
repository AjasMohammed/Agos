use crate::media::{gemini_user_parts, ImageResolver, NoopImageResolver};
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

/// How a Gemini model takes its reasoning dial, when it takes one at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiThinking {
    /// Gemini 3 and later: `thinkingConfig.thinkingLevel`, a rung.
    Level,
    /// Gemini 2.5: `thinkingConfig.thinkingBudget`, a token count, capped at
    /// the variant's own ceiling.
    Budget { ceiling: u32 },
}

/// Thinking-budget ceiling for 2.5 Pro. Over-range budgets are rejected.
const GEMINI_PRO_MAX_THINKING_BUDGET: u32 = 32_768;

/// Thinking-budget ceiling for every other 2.5 variant.
const GEMINI_FLASH_MAX_THINKING_BUDGET: u32 = 24_576;

/// Which reasoning dial `model` takes, or `None` when it takes none.
///
/// Gemini 400s on `thinkingConfig` sent to a model without a thinking mode,
/// so an unrecognised id deliberately gets nothing: a missing dial costs
/// reasoning depth, a wrong one costs the whole turn.
fn gemini_thinking(model: &str) -> Option<GeminiThinking> {
    // ponytail: name match, not a version parser — Gemini ids carry dotted
    // versions (`gemini-2.5-pro`) that a segment parser chokes on, and the
    // families that reason are a short list.

    // Image and speech variants carry the family version in their id but take
    // no `thinkingConfig`.
    if model.contains("-image") || model.contains("-tts") {
        return None;
    }
    if model.contains("gemini-3") {
        Some(GeminiThinking::Level)
    } else if model.contains("gemini-2.5") {
        Some(GeminiThinking::Budget {
            ceiling: if model.contains("pro") {
                GEMINI_PRO_MAX_THINKING_BUDGET
            } else {
                GEMINI_FLASH_MAX_THINKING_BUDGET
            },
        })
    } else {
        None
    }
}

/// Gemini 3's `thinkingLevel`, which has two rungs where the task definition
/// has five. An unrecognised rung sends nothing rather than guessing upward.
fn gemini_thinking_level(effort: &str) -> Option<&'static str> {
    match effort {
        "low" => Some("low"),
        "medium" | "high" | "xhigh" | "max" => Some("high"),
        other => {
            tracing::warn!(rung = %other, "unrecognised thinking effort; sending no thinkingConfig");
            None
        }
    }
}

/// The `generationConfig.thinkingConfig` value for this request, or `None`
/// when the model takes no dial or thinking is off.
///
/// `includeThoughts` makes the model return its reasoning as separate parts,
/// which `parse_gemini_parts` already keeps out of the answer text and the
/// streaming path already forwards as reasoning rather than content.
fn gemini_thinking_config(model: &str, options: &InferenceOptions) -> Option<Value> {
    let mut dial = match gemini_thinking(model)? {
        GeminiThinking::Level => {
            json!({ "thinkingLevel": gemini_thinking_level(options.thinking_effort.as_deref()?)? })
        }
        GeminiThinking::Budget { ceiling } => {
            json!({ "thinkingBudget": options.thinking_budget_tokens?.min(ceiling) })
        }
    };
    dial["includeThoughts"] = json!(true);
    Some(dial)
}

/// Gemini API adapter for Google models.
pub struct GeminiCore {
    client: Client,
    api_key: SecretString,
    model: String,
    capabilities: ModelCapabilities,
    pricing: ModelPricing,
    retry_policy: crate::retry::RetryPolicy,
    circuit_breaker: crate::retry::CircuitBreaker,
    /// In-flight cap for outbound requests, shared process-wide by every
    /// adapter pointed at the same upstream endpoint.
    concurrency: Arc<tokio::sync::Semaphore>,
    image_resolver: Arc<dyn ImageResolver>,
}

impl GeminiCore {
    pub fn new(api_key: SecretString, model: String) -> Self {
        let pricing = crate::lookup_pricing(&default_pricing_table(), "gemini", &model);
        // Per model, not per adapter: 2.0 and the image/TTS variants reject
        // `thinkingConfig` outright.
        let thinking = gemini_thinking(&model).is_some();
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(
                    crate::traits::DEFAULT_INFERENCE_TIMEOUT_SECS,
                ))
                .build()
                .expect("HTTP client TLS initialization failed"),
            api_key,
            model,
            capabilities: ModelCapabilities {
                context_window_tokens: 1_000_000,
                supports_images: true,
                supports_tool_calling: true,
                supports_json_mode: true,
                max_output_tokens: 0,
                supports_streaming: true,
                supports_parallel_tools: true,
                supports_prompt_caching: false,
                supports_thinking: thinking,
                supports_structured_output: true,
            },
            pricing,
            retry_policy: crate::retry::RetryPolicy::default(),
            circuit_breaker: crate::retry::CircuitBreaker::default(),
            // Gemini has no configurable base URL — every instance talks to
            // the one Google endpoint, so they all share one limiter.
            concurrency: crate::retry::concurrency_limiter_for(
                "https://generativelanguage.googleapis.com/v1beta",
            ),
            image_resolver: Arc::new(NoopImageResolver),
        }
    }

    pub fn with_image_resolver(mut self, resolver: Arc<dyn ImageResolver>) -> Self {
        self.image_resolver = resolver;
        self
    }

    /// Override the pricing for this adapter instance.
    pub fn with_pricing(mut self, pricing: ModelPricing) -> Self {
        self.pricing = pricing;
        self
    }

    fn format_contents(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut contents = Vec::new();
        // Gemini's contract: one user turn whose parts mirror the preceding model
        // turn's functionCall parts in execution order. Emitting each functionResponse
        // as its own user turn breaks correlation when the model calls the same tool
        // twice in one turn (same-name responses become indistinguishable). Accumulate
        // consecutive ToolResult entries here and flush as a single user turn when the
        // next non-tool-result entry arrives (or at end of loop).
        let mut pending_tool_response_parts: Vec<Value> = Vec::new();

        for entry in context.wire_entries().iter().map(|e| &**e) {
            // Flush pending native tool responses on any non-tool-result, non-system entry.
            if !matches!(entry.role, ContextRole::ToolResult | ContextRole::System)
                && !pending_tool_response_parts.is_empty()
            {
                contents.push(json!({
                    "role": "user",
                    "parts": std::mem::take(&mut pending_tool_response_parts),
                }));
            }

            match entry.role {
                ContextRole::System => continue, // System instructions are passed separately
                ContextRole::ToolResult => {
                    let tool_name = entry.metadata.as_ref().and_then(|m| m.tool_name.as_deref());

                    if let Some(name) = tool_name {
                        // Native Gemini functionResponse format.
                        // Parse content as JSON for structured response, fallback to wrapper.
                        let response_val = serde_json::from_str::<Value>(&entry.text())
                            .unwrap_or_else(|_| json!({"result": entry.text()}));
                        pending_tool_response_parts.push(json!({
                            "functionResponse": {
                                "name": name,
                                "response": response_val,
                            }
                        }));
                    } else {
                        // Legacy fallback — flush any pending native first, then emit text.
                        if !pending_tool_response_parts.is_empty() {
                            contents.push(json!({
                                "role": "user",
                                "parts": std::mem::take(&mut pending_tool_response_parts),
                            }));
                        }
                        contents.push(json!({
                            "role": "user",
                            "parts": [{"text": format!("Tool Result:\n{}", entry.text())}]
                        }));
                    }
                }
                ContextRole::User => {
                    let parts = gemini_user_parts(
                        entry,
                        self.capabilities.supports_images,
                        &self.image_resolver,
                    );
                    contents.push(json!({
                        "role": "user",
                        "parts": parts,
                    }));
                }
                ContextRole::Assistant => {
                    // If this assistant turn invoked tools, reconstruct the
                    // Gemini-native format with functionCall parts.
                    // Gemini requires functionResponse (user turn) to follow a
                    // model turn that contains the matching functionCall parts.
                    if let Some(Value::Array(calls)) = entry
                        .metadata
                        .as_ref()
                        .and_then(|m| m.assistant_tool_calls.as_ref())
                    {
                        let mut parts: Vec<Value> = Vec::new();
                        if !entry.text().is_empty() {
                            parts.push(json!({"text": entry.text()}));
                        }
                        for call in calls {
                            if let Some(name) = call.get("tool_name").and_then(|v| v.as_str()) {
                                let args =
                                    call.get("payload").cloned().unwrap_or_else(|| json!({}));
                                parts.push(json!({"functionCall": {"name": name, "args": args}}));
                            }
                        }
                        if parts.is_empty() {
                            parts.push(json!({"text": ""}));
                        }
                        contents.push(json!({"role": "model", "parts": parts}));
                    } else {
                        contents.push(json!({
                            "role": "model",
                            "parts": [{"text": entry.text()}]
                        }));
                    }
                }
            }
        }

        // Final flush — handles the common case where the last entries are tool results
        // (e.g. the kernel just dispatched a tool call and is about to ask Gemini for
        // the next inference).
        if !pending_tool_response_parts.is_empty() {
            contents.push(json!({
                "role": "user",
                "parts": pending_tool_response_parts,
            }));
        }

        contents
    }

    fn build_gemini_tools(tools: &[ToolManifest]) -> (Vec<Value>, HashMap<String, String>) {
        let mut function_declarations = Vec::new();
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

            function_declarations.push(json!({
                "name": tool_name,
                "description": manifest.manifest.description,
                "parameters": tool_helpers::normalize_tool_input_schema_with_examples(manifest.payload_schema.as_ref(), &manifest.examples),
            }));
        }

        (function_declarations, intent_by_tool)
    }

    fn parse_gemini_tool_calls(
        parts: &[Value],
        intent_by_tool: &HashMap<String, String>,
    ) -> (String, Vec<InferenceToolCall>) {
        let mut text = String::new();
        let mut tool_calls = Vec::new();

        for part in parts {
            let is_thought = part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            // Only include text from non-thought parts — Gemini thinking
            // models set `thought: true` on internal reasoning parts.
            if !is_thought {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            // Always check for functionCall, even on thought parts, so
            // tool calls are never silently dropped.
            if let Some(fc) = part.get("functionCall").and_then(Value::as_object) {
                let Some(tool_name) = fc
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                else {
                    continue;
                };

                let payload = tool_helpers::validate_payload_object(
                    tool_name,
                    "gemini",
                    fc.get("args").cloned(),
                );

                if !tool_helpers::check_payload_size(tool_name, &payload) {
                    continue;
                }

                let intent_type = intent_by_tool
                    .get(tool_name)
                    .cloned()
                    .unwrap_or_else(|| "query".to_string());

                tool_calls.push(InferenceToolCall {
                    id: None, // Gemini does not use tool call IDs
                    tool_name: tool_name.to_string(),
                    intent_type,
                    payload,
                });
            }
        }

        (text, tool_calls)
    }
}

#[async_trait]
impl LLMCore for GeminiCore {
    fn supports_native_tool_calling(&self) -> bool {
        true
    }

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
                provider: "gemini".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            self.model,
        );

        let prepared = crate::media::prepare_for_inference(
            context,
            self.capabilities.supports_images,
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
        let contents = self.format_contents(context);

        // If options disable tools, exclude them.
        let effective_tools = if matches!(options.tool_choice, Some(ToolChoice::None)) {
            &[][..]
        } else {
            tools
        };
        let (function_declarations, intent_by_tool) = Self::build_gemini_tools(effective_tools);

        let mut body = json!({
            "contents": contents,
        });

        let active = context.active_entries();
        // ALL system entries joined — a `.find()` here dropped the
        // `<agent-context-memory>` block and the memory nudge on every turn.
        let sys = active
            .iter()
            .filter(|e| e.role == ContextRole::System)
            .map(|e| e.text())
            // Drop blank entries so N empty System entries can't join into
            // "\n\n", which is not `is_empty()` and would emit a whitespace
            // `systemInstruction` where `.find()` previously omitted the key.
            .filter(|t| !t.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !sys.is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": sys}]});
        }

        if !function_declarations.is_empty() {
            body["tools"] = json!([{"functionDeclarations": function_declarations}]);
            // Apply tool_choice via functionCallingConfig.
            // ToolChoice::Specific constrains to a named function via allowedFunctionNames.
            body["toolConfig"] = match &options.tool_choice {
                Some(ToolChoice::None) => {
                    // excluded above; defensive
                    json!({"functionCallingConfig": {"mode": "NONE"}})
                }
                Some(ToolChoice::Required) => {
                    json!({"functionCallingConfig": {"mode": "ANY"}})
                }
                Some(ToolChoice::Specific(name)) => {
                    json!({"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": [name]}})
                }
                Some(ToolChoice::Auto) | None => {
                    json!({"functionCallingConfig": {"mode": "AUTO"}})
                }
            };
        }

        // Apply generation config options.
        let mut gen_config = serde_json::Map::new();
        if let Some(temp) = options.temperature {
            gen_config.insert("temperature".to_string(), json!(temp));
        }
        if let Some(max_tok) = options.max_tokens {
            gen_config.insert("maxOutputTokens".to_string(), json!(max_tok));
        }
        if options.json_mode {
            gen_config.insert("responseMimeType".to_string(), json!("application/json"));
        }
        if let Some(dial) = gemini_thinking_config(&self.model, options) {
            gen_config.insert("thinkingConfig".to_string(), dial);
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = Value::Object(gen_config);
        }

        // `_permit` holds the endpoint's concurrency slot until this scope
        // ends, i.e. until the (non-streamed) body has been read.
        let (res, _permit) = crate::retry::send_with_retry(
            "gemini",
            &self.retry_policy,
            &self.circuit_breaker,
            Some(&self.concurrency),
            || {
                self.client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("x-goog-api-key", self.api_key.expose_secret())
                    .json(&body)
            },
        )
        .await?;

        let json_resp: serde_json::Value =
            res.json().await.map_err(|e| AgentOSError::LLMError {
                provider: "gemini".to_string(),
                reason: format!("Failed to parse JSON response: {}", e),
            })?;

        let parts = json_resp["candidates"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(|c| c["content"]["parts"].as_array())
            .cloned()
            .unwrap_or_default();

        let (text, tool_calls) = Self::parse_gemini_tool_calls(&parts, &intent_by_tool);

        let finish_reason = json_resp["candidates"][0]["finishReason"]
            .as_str()
            .unwrap_or("STOP");
        let stop_reason = match finish_reason {
            "STOP" => StopReason::EndTurn,
            "FUNCTION_CALL" => StopReason::ToolUse,
            "MAX_TOKENS" => StopReason::MaxTokens,
            "SAFETY" => StopReason::ContentFilter,
            "STOP_SEQUENCE" => StopReason::StopSequence,
            other => StopReason::Other(other.to_string()),
        };

        let prompt_tokens = json_resp["usageMetadata"]["promptTokenCount"]
            .as_u64()
            .unwrap_or(0);
        let completion_tokens = json_resp["usageMetadata"]["candidatesTokenCount"]
            .as_u64()
            .unwrap_or(0);
        let total_tokens = json_resp["usageMetadata"]["totalTokenCount"]
            .as_u64()
            .unwrap_or(prompt_tokens + completion_tokens);
        let cached_tokens = json_resp["usageMetadata"]["cachedContentTokenCount"]
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
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}",
            self.model,
        );
        match self
            .client
            .get(&url)
            .header("x-goog-api-key", self.api_key.expose_secret())
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
        "gemini"
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
                provider: "gemini".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        // Gemini streaming uses streamGenerateContent with alt=sse.
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?alt=sse",
            self.model,
        );

        let prepared = crate::media::prepare_for_inference(
            context,
            self.capabilities.supports_images,
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
        let contents = self.format_contents(context);
        let (function_declarations, intent_by_tool) = Self::build_gemini_tools(tools);

        let mut body = json!({ "contents": contents });

        let active = context.active_entries();
        let sys = active
            .iter()
            .filter(|e| e.role == ContextRole::System)
            .map(|e| e.text())
            // Drop blank entries so N empty System entries can't join into
            // "\n\n", which is not `is_empty()` and would emit a whitespace
            // `systemInstruction` where `.find()` previously omitted the key.
            .filter(|t| !t.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !sys.is_empty() {
            body["systemInstruction"] = json!({ "parts": [{"text": sys}] });
        }
        if !function_declarations.is_empty() {
            body["tools"] = json!([{ "functionDeclarations": function_declarations }]);
        }

        // Retry the initial POST + status check (before any SSE event is
        // forwarded) so a transient upstream 5xx / network blip doesn't fail
        // the whole chat turn — matching the resilience of the non-streaming
        // path. `send_with_retry` returns the live `Response` with its body
        // stream intact on 2xx, along with the endpoint concurrency permit.
        // `_permit` is kept alive for the whole of this function so the slot
        // covers token generation: on a streamed request the headers arrive
        // at the *first* token, so releasing it here would leave chat — the
        // busiest caller — outside the cap entirely.
        let res = crate::retry::send_with_retry(
            "gemini",
            &self.retry_policy,
            &self.circuit_breaker,
            Some(&self.concurrency),
            || {
                self.client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("x-goog-api-key", self.api_key.expose_secret())
                    .json(&body)
            },
        )
        .await;
        let (res, _permit) = match res {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(InferenceEvent::Error(e.to_string())).await;
                return Err(e);
            }
        };

        let mut full_text = String::new();
        let mut tool_calls: Vec<InferenceToolCall> = Vec::new();
        let mut usage = TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        let mut cached_tokens: u64 = 0;
        let mut stop_reason = StopReason::EndTurn;
        let mut line_buffer = String::new();
        const MAX_LINE_BUFFER_BYTES: usize = 1_048_576; // 1 MB

        // Carry buffer for a multibyte UTF-8 sequence split across HTTP chunks.
        let mut utf8_pending: Vec<u8> = Vec::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "gemini".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            crate::streaming_helpers::push_utf8_chunk(&mut utf8_pending, &chunk, &mut line_buffer);

            if line_buffer.len() > MAX_LINE_BUFFER_BYTES {
                let err_msg = "SSE line buffer exceeded 1 MB";
                let _ = tx.send(InferenceEvent::Error(err_msg.to_string())).await;
                return Err(AgentOSError::LLMError {
                    provider: "gemini".to_string(),
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
                let Ok(data_json) = serde_json::from_str::<Value>(data) else {
                    continue;
                };

                // Extract parts from candidate.
                let parts = data_json["candidates"]
                    .as_array()
                    .and_then(|c| c.first())
                    .and_then(|c| c["content"]["parts"].as_array())
                    .cloned()
                    .unwrap_or_default();

                for part in &parts {
                    let is_thought = part
                        .get("thought")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);

                    // Thought parts go out as reasoning, never as answer text.
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            if is_thought {
                                let _ = tx.send(InferenceEvent::Thinking(t.to_string())).await;
                            } else {
                                full_text.push_str(t);
                                let _ = tx.send(InferenceEvent::Token(t.to_string())).await;
                            }
                        }
                    }
                    // Always check for functionCall, even on thought parts.
                    // Gemini sends functionCall as complete objects, not streamed.
                    if let Some(fc) = part.get("functionCall").and_then(Value::as_object) {
                        if let Some(tool_name) = fc
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|n| !n.is_empty())
                        {
                            let payload = tool_helpers::validate_payload_object(
                                tool_name,
                                "gemini",
                                fc.get("args").cloned(),
                            );
                            if tool_helpers::check_payload_size(tool_name, &payload) {
                                let intent_type = intent_by_tool
                                    .get(tool_name)
                                    .cloned()
                                    .unwrap_or_else(|| "query".to_string());
                                let tc = InferenceToolCall {
                                    id: None,
                                    tool_name: tool_name.to_string(),
                                    intent_type,
                                    payload,
                                };
                                let _ = tx.send(InferenceEvent::ToolCallComplete(tc.clone())).await;
                                tool_calls.push(tc);
                            }
                        }
                    }
                }

                // Finish reason.
                if let Some(reason) = data_json["candidates"]
                    .as_array()
                    .and_then(|c| c.first())
                    .and_then(|c| c["finishReason"].as_str())
                {
                    stop_reason = match reason {
                        "STOP" => StopReason::EndTurn,
                        "FUNCTION_CALL" => StopReason::ToolUse,
                        "MAX_TOKENS" => StopReason::MaxTokens,
                        "SAFETY" => StopReason::ContentFilter,
                        "STOP_SEQUENCE" => StopReason::StopSequence,
                        other => StopReason::Other(other.to_string()),
                    };
                }

                // Usage metadata.
                if let Some(um) = data_json.get("usageMetadata") {
                    usage.prompt_tokens = um["promptTokenCount"]
                        .as_u64()
                        .unwrap_or(usage.prompt_tokens);
                    usage.completion_tokens = um["candidatesTokenCount"]
                        .as_u64()
                        .unwrap_or(usage.completion_tokens);
                    usage.total_tokens = um["totalTokenCount"]
                        .as_u64()
                        .unwrap_or(usage.prompt_tokens + usage.completion_tokens);
                    cached_tokens = um["cachedContentTokenCount"]
                        .as_u64()
                        .unwrap_or(cached_tokens);
                    let _ = tx.send(InferenceEvent::Usage(usage.clone())).await;
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
    fn test_format_contents_gemini() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            parts: vec![ContentPart::Text {
                text: "System".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![ContentPart::Text {
                text: "User".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::Assistant,
            parts: vec![ContentPart::Text {
                text: "Assistant".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = GeminiCore::new(SecretString::new("fake".into()), "gemini".into());
        let contents = adapter.format_contents(&ctx);

        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "User");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["text"], "Assistant");
    }

    #[test]
    fn test_parse_gemini_tool_calls_extracts_function_call() {
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());

        let parts = vec![
            json!({"text": "Reading the file now."}),
            json!({
                "functionCall": {
                    "name": "file-reader",
                    "args": {"path": "test.txt"}
                }
            }),
        ];

        let (text, tool_calls) = GeminiCore::parse_gemini_tool_calls(&parts, &intent_map);
        assert_eq!(text, "Reading the file now.");
        assert_eq!(tool_calls.len(), 1);
        assert!(tool_calls[0].id.is_none());
        assert_eq!(tool_calls[0].tool_name, "file-reader");
        assert_eq!(tool_calls[0].intent_type, "read");
        assert_eq!(tool_calls[0].payload["path"], "test.txt");
    }

    #[test]
    fn test_parse_gemini_tool_calls_text_only() {
        let parts = vec![json!({"text": "Done."})];
        let (text, tool_calls) = GeminiCore::parse_gemini_tool_calls(&parts, &HashMap::new());
        assert_eq!(text, "Done.");
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_format_contents_native_tool_result() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(crate::tool_call_turn(&[("call_1", "file-reader")]));
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: r#"{"status": "ok"}"#.to_string(),
            }],
            metadata: Some(ContextMetadata {
                tool_name: Some("file-reader".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("call_1".to_string()),
                assistant_tool_calls: None,
            }),
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = GeminiCore::new(SecretString::new("fake".into()), "gemini".into());
        let contents = adapter.format_contents(&ctx);

        assert_eq!(contents.len(), 2);
        assert_eq!(contents[1]["role"], "user");
        let parts = contents[1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        let fr = &parts[0]["functionResponse"];
        assert_eq!(fr["name"], "file-reader");
        assert_eq!(fr["response"]["status"], "ok");
    }

    #[test]
    fn test_format_contents_native_tool_result_plain_text() {
        // Non-JSON content wraps in {"result": "..."}
        let mut ctx = ContextWindow::new(5);
        ctx.push(crate::tool_call_turn(&[("call_2", "shell")]));
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: "plain text result".to_string(),
            }],
            metadata: Some(ContextMetadata {
                tool_name: Some("shell".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("call_2".to_string()),
                assistant_tool_calls: None,
            }),
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = GeminiCore::new(SecretString::new("fake".into()), "gemini".into());
        let contents = adapter.format_contents(&ctx);

        let fr = &contents[1]["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "shell");
        assert_eq!(fr["response"]["result"], "plain text result");
    }

    /// Test the stop reason mapping used in infer_with_tools.
    #[test]
    fn test_gemini_stop_reason_mapping() {
        let cases = vec![
            ("STOP", StopReason::EndTurn),
            ("FUNCTION_CALL", StopReason::ToolUse),
            ("MAX_TOKENS", StopReason::MaxTokens),
            ("SAFETY", StopReason::ContentFilter),
            ("STOP_SEQUENCE", StopReason::StopSequence),
            ("OTHER_REASON", StopReason::Other("OTHER_REASON".into())),
        ];
        for (input, expected) in cases {
            let result = match input {
                "STOP" => StopReason::EndTurn,
                "FUNCTION_CALL" => StopReason::ToolUse,
                "MAX_TOKENS" => StopReason::MaxTokens,
                "SAFETY" => StopReason::ContentFilter,
                "STOP_SEQUENCE" => StopReason::StopSequence,
                other => StopReason::Other(other.to_string()),
            };
            assert_eq!(result, expected, "Failed for input: {input}");
        }
    }

    #[test]
    fn test_parse_gemini_tool_calls_skips_thought_parts() {
        let parts = vec![
            json!({"text": "internal reasoning about the problem", "thought": true}),
            json!({"text": "visible answer"}),
        ];
        let (text, tool_calls) = GeminiCore::parse_gemini_tool_calls(&parts, &HashMap::new());
        assert_eq!(text, "visible answer");
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_parse_gemini_tool_calls_thought_false_not_filtered() {
        let parts = vec![
            json!({"text": "normal text", "thought": false}),
            json!({"text": " more text"}),
        ];
        let (text, _) = GeminiCore::parse_gemini_tool_calls(&parts, &HashMap::new());
        assert_eq!(text, "normal text more text");
    }

    #[test]
    fn test_parse_gemini_tool_calls_mixed_thought_and_function_call() {
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());

        let parts = vec![
            json!({"text": "let me think about this...", "thought": true}),
            json!({"functionCall": {"name": "file-reader", "args": {"path": "test.txt"}}}),
        ];
        let (text, tool_calls) = GeminiCore::parse_gemini_tool_calls(&parts, &intent_map);
        assert!(text.is_empty(), "Thought text should be filtered");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool_name, "file-reader");
        assert_eq!(tool_calls[0].intent_type, "read");
    }

    #[test]
    fn test_parse_gemini_all_thought_parts_results_in_empty_text() {
        let parts = vec![
            json!({"text": "first thought", "thought": true}),
            json!({"text": "second thought", "thought": true}),
        ];
        let (text, tool_calls) = GeminiCore::parse_gemini_tool_calls(&parts, &HashMap::new());
        assert!(text.is_empty());
        assert!(tool_calls.is_empty());
    }

    /// Golden-body assertion: Gemini's generateContent API requires
    /// `tools[0].functionDeclarations[]` where each entry is
    /// `{name, description, parameters}` — same JSON Schema role as
    /// OpenAI's `parameters` / Anthropic's `input_schema`.
    #[test]
    fn test_build_gemini_tools_uses_parameters_key() {
        use agentos_types::tool::{
            ToolCapabilities, ToolExecutor, ToolInfo, ToolOutputs, ToolSchema,
        };
        let manifest = ToolManifest {
            manifest: ToolInfo {
                category: None,
                search_hints: vec![],
                name: "file-reader".to_string(),
                version: "1.0.0".to_string(),
                description: "Read a file".to_string(),
                author: "core".to_string(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier: TrustTier::Core,
                tags: None,
                capability_tags: vec![],
                group: String::new(),
            },
            capabilities_required: ToolCapabilities {
                permissions: vec!["fs.user_data:r".to_string()],
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "Input".to_string(),
                output: "Output".to_string(),
            },
            payload_schema: Some(
                json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            ),
            examples: vec![],
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
            fallbacks: vec![],
            risk_class: Default::default(),
            risk_class_by_action: Default::default(),
            usage_hints: None,
            tags: vec![],
        };

        let (decls, _) = GeminiCore::build_gemini_tools(&[manifest]);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "file-reader");
        assert!(
            decls[0].get("parameters").is_some(),
            "Gemini functionDeclaration must use `parameters` key; got: {}",
            decls[0]
        );
        assert_eq!(decls[0]["parameters"]["type"], "object");
    }

    /// Regression test for the same-name parallel `functionCall` correlation
    /// bug: when the model emits two ToolResults consecutively (matching a
    /// preceding assistant turn that called the same tool twice), they MUST
    /// be coalesced into a single user turn carrying two `functionResponse`
    /// parts in execution order. Splitting them into separate user turns
    /// breaks Gemini's positional correlation when the tool name is the same
    /// (and is non-canonical even when names differ).
    #[test]
    fn test_format_contents_coalesces_consecutive_tool_results() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![ContentPart::Text {
                text: "Run both reads".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        // Assistant turn invokes file-reader twice.
        ctx.push(ContextEntry {
            role: ContextRole::Assistant,
            parts: vec![ContentPart::Text {
                text: String::new(),
            }],
            metadata: Some(ContextMetadata {
                tool_name: None,
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: None,
                assistant_tool_calls: Some(serde_json::json!([
                    {"id": "call_0", "tool_name": "file-reader", "payload": {"path": "/a"}},
                    {"id": "call_1", "tool_name": "file-reader", "payload": {"path": "/b"}},
                ])),
            }),
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
        // Two consecutive ToolResults — must coalesce into ONE user turn.
        for (i, body) in [r#"{"contents": "A"}"#, r#"{"contents": "B"}"#]
            .iter()
            .enumerate()
        {
            ctx.push(ContextEntry {
                role: ContextRole::ToolResult,
                parts: vec![ContentPart::Text {
                    text: body.to_string(),
                }],
                metadata: Some(ContextMetadata {
                    tool_name: Some("file-reader".to_string()),
                    tool_id: None,
                    intent_id: None,
                    tokens_estimated: None,
                    tool_call_id: Some(format!("call_{i}")),
                    assistant_tool_calls: None,
                }),
                timestamp: chrono::Utc::now(),
                importance: 0.5,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::Active,
                category: ContextCategory::History,
                is_summary: false,
            });
        }

        let adapter = GeminiCore::new(SecretString::new("fake".into()), "gemini".into());
        let contents = adapter.format_contents(&ctx);

        // Expected shape: user (prompt) → model (two functionCalls) → user (two functionResponses)
        assert_eq!(
            contents.len(),
            3,
            "Two ToolResults must collapse to ONE user turn; got {} entries",
            contents.len()
        );
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");

        let response_parts = contents[2]["parts"].as_array().expect("parts is array");
        assert_eq!(
            response_parts.len(),
            2,
            "Coalesced user turn must carry two functionResponse parts in execution order"
        );
        assert_eq!(response_parts[0]["functionResponse"]["name"], "file-reader");
        assert_eq!(
            response_parts[0]["functionResponse"]["response"]["contents"],
            "A"
        );
        assert_eq!(response_parts[1]["functionResponse"]["name"], "file-reader");
        assert_eq!(
            response_parts[1]["functionResponse"]["response"]["contents"],
            "B"
        );
    }

    /// `thinkingConfig` is a 400 on a model with no thinking mode, and the two
    /// shapes are not interchangeable, so the gate must be exact and must say
    /// "nothing" for ids it does not recognise.
    #[test]
    fn thinking_shape_is_per_family_and_absent_for_unknown_ids() {
        assert_eq!(
            gemini_thinking("gemini-3-pro-preview"),
            Some(GeminiThinking::Level)
        );
        assert_eq!(
            gemini_thinking("gemini-2.5-flash"),
            Some(GeminiThinking::Budget {
                ceiling: GEMINI_FLASH_MAX_THINKING_BUDGET
            })
        );
        assert_eq!(
            gemini_thinking("gemini-2.5-pro"),
            Some(GeminiThinking::Budget {
                ceiling: GEMINI_PRO_MAX_THINKING_BUDGET
            })
        );
        assert_eq!(gemini_thinking("gemini-2.0-flash"), None);
        assert_eq!(gemini_thinking("gemini-1.5-pro"), None);
        assert_eq!(gemini_thinking("some-custom-alias"), None);
        // Carry a thinking family's version but take no thinkingConfig.
        assert_eq!(gemini_thinking("gemini-2.5-flash-image"), None);
        assert_eq!(gemini_thinking("gemini-2.5-flash-preview-tts"), None);
    }

    /// The `max` thinking level asks for 100 000 tokens; 2.5 Flash rejects
    /// anything over 24 576 and Pro anything over 32 768, so the budget has to
    /// clamp to the ceiling of the variant actually named.
    #[test]
    fn thinking_budget_clamps_to_the_variants_own_ceiling() {
        let opts = |budget: u32| InferenceOptions {
            thinking_budget_tokens: Some(budget),
            thinking_effort: Some("max".to_string()),
            ..Default::default()
        };

        let flash = gemini_thinking_config("gemini-2.5-flash", &opts(100_000)).unwrap();
        assert_eq!(flash["thinkingBudget"], GEMINI_FLASH_MAX_THINKING_BUDGET);
        assert_eq!(flash["includeThoughts"], true);

        let pro = gemini_thinking_config("gemini-2.5-pro", &opts(100_000)).unwrap();
        assert_eq!(pro["thinkingBudget"], GEMINI_PRO_MAX_THINKING_BUDGET);

        // Under the ceiling the budget passes through untouched.
        let low = gemini_thinking_config("gemini-2.5-flash", &opts(1_024)).unwrap();
        assert_eq!(low["thinkingBudget"], 1_024);
    }

    /// The two shapes are not interchangeable — a budget sent to Gemini 3 or a
    /// level sent to 2.5 is a 400 — and thinking-off must send neither.
    #[test]
    fn thinking_config_shape_follows_the_family_and_is_absent_when_off() {
        let all_rungs = |effort: &str| InferenceOptions {
            thinking_budget_tokens: Some(8_192),
            thinking_effort: Some(effort.to_string()),
            ..Default::default()
        };

        let three = gemini_thinking_config("gemini-3-pro-preview", &all_rungs("max")).unwrap();
        assert_eq!(three["thinkingLevel"], "high");
        assert!(three.get("thinkingBudget").is_none());
        assert_eq!(
            gemini_thinking_config("gemini-3-pro-preview", &all_rungs("low")).unwrap()
                ["thinkingLevel"],
            "low"
        );

        let two_five = gemini_thinking_config("gemini-2.5-flash", &all_rungs("max")).unwrap();
        assert!(two_five.get("thinkingLevel").is_none());
        assert_eq!(two_five["thinkingBudget"], 8_192);

        // Thinking off: both fields are `None`, so nothing goes out.
        assert!(gemini_thinking_config("gemini-2.5-flash", &InferenceOptions::default()).is_none());
        assert!(
            gemini_thinking_config("gemini-3-pro-preview", &InferenceOptions::default()).is_none()
        );
        // Unrecognised rung on the level path sends nothing rather than `high`.
        assert!(gemini_thinking_config("gemini-3-pro-preview", &all_rungs("minimal")).is_none());
    }
}
