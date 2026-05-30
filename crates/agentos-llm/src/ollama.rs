use crate::media::{ImageResolver, NoopImageResolver};
use crate::tool_helpers;
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
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
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
    /// Per-instance in-flight cap for outbound requests.
    concurrency: Arc<tokio::sync::Semaphore>,
    image_resolver: Arc<dyn ImageResolver>,
    /// Model IDs that accept `images: [...]` on user messages (e.g. `llava`). Empty → no vision.
    vision_models: Vec<String>,
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
            concurrency: crate::retry::default_concurrency_limiter(),
            image_resolver: Arc::new(NoopImageResolver),
            vision_models: Vec::new(),
        }
    }

    /// Override the pricing for this adapter instance.
    pub fn with_pricing(mut self, pricing: ModelPricing) -> Self {
        self.pricing = pricing;
        self
    }

    /// Inject an image resolver so multimodal context entries can be
    /// converted to base64 payloads. Defaults to `NoopImageResolver`.
    pub fn with_image_resolver(mut self, resolver: Arc<dyn ImageResolver>) -> Self {
        self.image_resolver = resolver;
        self
    }

    /// Opt-in vision: when `model` matches an entry (or the list contains `"*"`),
    /// [`LLMCore::supports_images`] is true and user turns emit Ollama `images` base64 payloads.
    pub fn with_vision_models(mut self, models: Vec<String>) -> Self {
        self.vision_models = models;
        self
    }

    fn model_has_vision(&self) -> bool {
        if self.vision_models.is_empty() {
            return false;
        }
        let m = self.model.as_str();
        self.vision_models
            .iter()
            .any(|vm| vm.as_str() == "*" || vm == m)
    }

    fn ollama_user_from_entry(&self, entry: &ContextEntry) -> OllamaChatMessage {
        let vision = self.model_has_vision();
        let mut text_buf = String::new();
        let mut images: Vec<String> = Vec::new();
        for p in &entry.parts {
            match p {
                ContentPart::Text { text } => text_buf.push_str(text),
                ContentPart::Image { mime, source } => {
                    if vision {
                        match crate::media::resolve_image_to_base64(
                            mime,
                            source,
                            &self.image_resolver,
                        ) {
                            Ok((_, b64)) => images.push(b64),
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "ollama: failed to resolve image for message"
                                );
                                text_buf.push_str(&format!(" [[image error: {e}]]"));
                            }
                        }
                    } else {
                        text_buf.push_str(&crate::media::image_fallback_stub("attachment", mime));
                    }
                }
            }
        }
        OllamaChatMessage {
            role: "user".to_string(),
            content: text_buf,
            thinking: None,
            tool_calls: Vec::new(),
            request_tool_calls: None,
            images: if images.is_empty() {
                None
            } else {
                Some(images)
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
                            content: entry.text(),
                            thinking: None,
                            tool_calls: Vec::new(),
                            request_tool_calls,
                            images: None,
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
                            ("tool".to_string(), entry.text())
                        } else {
                            (
                                "user".to_string(),
                                format!("Tool Result:\n{}", entry.text()),
                            )
                        };
                        OllamaChatMessage {
                            role,
                            content,
                            thinking: None,
                            tool_calls: Vec::new(),
                            request_tool_calls: None,
                            images: None,
                        }
                    }
                    ContextRole::System => OllamaChatMessage {
                        role: "system".to_string(),
                        content: entry.text(),
                        thinking: None,
                        tool_calls: Vec::new(),
                        request_tool_calls: None,
                        images: None,
                    },
                    ContextRole::User => self.ollama_user_from_entry(entry),
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
            Some(&self.concurrency),
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
        intent_by_tool: &HashMap<String, String>,
    ) -> InferenceResult {
        let tool_calls: Vec<InferenceToolCall> = ollama_response
            .message
            .tool_calls
            .into_iter()
            .filter_map(|tc| {
                let payload = tool_helpers::validate_payload_object(
                    &tc.function.name,
                    "ollama",
                    Some(tc.function.arguments),
                );
                if !tool_helpers::check_payload_size(&tc.function.name, &payload) {
                    return None;
                }
                Some(InferenceToolCall {
                    id: None,
                    tool_name: tc.function.name,
                    intent_type: "execute".to_string(),
                    payload,
                })
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

        // When content is empty but thinking has content, use thinking as
        // fallback text. This prevents empty responses from thinking models
        // (Kimi, Gemma4, DeepSeek) that put their entire reply in the
        // thinking field for simple follow-up questions.
        let has_thinking = ollama_response
            .message
            .thinking
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty());
        let text = if ollama_response.message.content.trim().is_empty()
            && has_thinking
            && tool_calls.is_empty()
        {
            let thinking = ollama_response.message.thinking.as_deref().unwrap_or("");
            tracing::info!(
                model = %self.model,
                thinking_len = thinking.len(),
                tool_calls_count = tool_calls.len(),
                "Ollama content empty but thinking present — using thinking as fallback text"
            );
            thinking.to_string()
        } else {
            ollama_response.message.content.clone()
        };

        // Log when the model returns empty content — helps diagnose empty-response issues.
        if text.trim().is_empty() {
            tracing::warn!(
                model = %self.model,
                done = ollama_response.done,
                done_reason = ?ollama_response.done_reason,
                prompt_eval_count = ?ollama_response.prompt_eval_count,
                eval_count = ?ollama_response.eval_count,
                tool_calls_count = tool_calls.len(),
                stop_reason = ?stop_reason,
                has_thinking = has_thinking,
                "Ollama returned empty message content"
            );
        } else {
            tracing::debug!(
                model = %self.model,
                content_len = text.len(),
                done_reason = ?ollama_response.done_reason,
                eval_count = ?ollama_response.eval_count,
                tool_calls_count = tool_calls.len(),
                has_thinking = has_thinking,
                "Ollama response received"
            );
        }

        // Small-model fallback: recover tool calls embedded as fenced JSON in
        // `content` when Ollama didn't populate the native `tool_calls` array.
        let (mut tool_calls, text, stop_reason) = if tool_calls.is_empty() && !text.is_empty() {
            let recovered = Self::parse_tool_calls_from_text(&text, intent_by_tool);
            if !recovered.is_empty() {
                let stripped = tool_helpers::strip_tool_json_fences(&text, recovered.len());
                (recovered, stripped, StopReason::ToolUse)
            } else {
                (tool_calls, text, stop_reason)
            }
        } else {
            (tool_calls, text, stop_reason)
        };

        // Assign synthetic ids to native Ollama tool calls (Ollama omits ids).
        for (i, tc) in tool_calls.iter_mut().enumerate() {
            if tc.id.is_none() {
                tc.id = Some(format!(
                    "ollama_{}_{}_{i}",
                    tc.tool_name,
                    chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
                ));
            }
        }

        InferenceResult {
            text,
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

    /// Recover tool calls from fenced JSON blocks in model text.
    /// Same algorithm as `CustomCore::parse_tool_calls_from_text`.
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
                    .unwrap_or(serde_json::json!({}));
                let payload =
                    tool_helpers::validate_payload_object(tool_name, "ollama", Some(payload));
                if !tool_helpers::check_payload_size(tool_name, &payload) {
                    continue;
                }
                let intent_type = obj
                    .get("intent_type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| intent_by_tool.get(tool_name).cloned())
                    .unwrap_or_else(|| "query".to_string());
                let synthetic_id = format!(
                    "fallback_{}_{}_{}",
                    tool_name,
                    chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
                    out.len()
                );
                tracing::warn!(
                    tool = tool_name,
                    "Recovered tool call from fenced JSON in Ollama text (small-model fallback)"
                );
                out.push(InferenceToolCall {
                    id: Some(synthetic_id),
                    tool_name: tool_name.to_string(),
                    intent_type,
                    payload,
                });
            }
        }
        out
    }
}

/// Enrich a raw Ollama error reason with actionable, copy-pasteable hints.
///
/// `gemma4:31b-cloud` and other Ollama Cloud models return 403 with a
/// `subscription`/`upgrade` body when the user is on the free tier. The generic
/// `API error 403 Forbidden: {...json...}` message produced by `send_with_retry`
/// buries that signal, so onboarding prompts surface a bare "Task failed"
/// without telling the user the model itself is paywalled. This helper inspects
/// the already-formatted reason string (which embeds the status and body) and
/// prepends a short hint when it recognises the pattern, returning the reason
/// unchanged otherwise.
fn friendly_ollama_reason(reason: String) -> String {
    let lc = reason.to_ascii_lowercase();
    // Anchor on the `API error {status}` segment that `send_with_retry` emits
    // for non-retryable errors (retry.rs), NOT a bare status-number substring:
    // 401/403/404 are all non-retryable so they always arrive in that form,
    // and matching the prefix avoids mis-rewriting (say) a 500 whose body
    // merely mentions "401" or "404" in prose or an ID.
    if lc.contains("api error 403") && (lc.contains("subscription") || lc.contains("upgrade")) {
        return format!(
            "Ollama Cloud model requires a paid subscription. \
             Either subscribe at https://ollama.com/upgrade, \
             switch to a local model in `agentos config set llm.model <name>`, \
             or pick another provider. (raw: {reason})"
        );
    }
    if lc.contains("api error 401") {
        return format!(
            "Ollama rejected the request as unauthenticated. \
             Verify the API key / host setting in `config/default.toml`. (raw: {reason})"
        );
    }
    if lc.contains("api error 404") && lc.contains("model") {
        return format!(
            "Ollama reports the model is not installed. \
             Run `ollama pull <model>` or pick an installed model with \
             `agentos config set llm.model <name>`. (raw: {reason})"
        );
    }
    reason
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
    /// Thinking/reasoning content from models that support chain-of-thought
    /// (e.g. Kimi, Gemma4, DeepSeek). Ollama streams this in a separate field
    /// from `content`. Captured so it can be used as fallback text when
    /// `content` is empty, and for diagnostic logging.
    #[serde(default, skip_serializing)]
    thinking: Option<String>,
    /// Inbound tool calls deserialized from model responses. Never serialized
    /// outbound (use `request_tool_calls` for that instead).
    ///
    /// SAFETY: Both this field and `request_tool_calls` map to the JSON key
    /// `"tool_calls"`. This works because the two fields have complementary
    /// skip annotations: this field has `skip_serializing` (deserialize-only)
    /// and `request_tool_calls` has `skip_deserializing` (serialize-only).
    /// Serde resolves the name collision because at most one field participates
    /// in each direction. If serde's derive behavior changes, split into
    /// separate request/response structs.
    #[serde(default, skip_serializing)]
    tool_calls: Vec<OllamaResponseToolCall>,
    /// Outbound tool calls for prior assistant messages in multi-turn context.
    /// Serialized as `"tool_calls"` (Ollama/OpenAI-compatible format); skipped
    /// when None so non-tool-call messages stay minimal.
    /// See safety note on `tool_calls` above.
    #[serde(
        rename = "tool_calls",
        skip_serializing_if = "Option::is_none",
        skip_deserializing
    )]
    request_tool_calls: Option<Vec<serde_json::Value>>,
    /// Base64-encoded raw image bytes for vision models (user role only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
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
    fn supports_images(&self) -> bool {
        self.model_has_vision()
    }

    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let start = std::time::Instant::now();

        // Convert ContextWindow to Ollama chat messages format
        let prepared = crate::media::prepare_for_inference(
            context,
            self.model_has_vision(),
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
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
        Ok(self.response_to_inference_result(ollama_response, duration_ms, &HashMap::new()))
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
        let prepared = crate::media::prepare_for_inference(
            context,
            self.model_has_vision(),
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
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
                    parameters: tool_helpers::normalize_tool_input_schema(
                        t.payload_schema.as_ref(),
                    ),
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

        let intent_by_tool: HashMap<String, String> = effective_tools
            .iter()
            .map(|t| {
                let intent = tool_helpers::infer_intent_type_from_permissions(
                    &t.capabilities_required.permissions,
                );
                (t.manifest.name.clone(), intent)
            })
            .collect();

        let ollama_response = self.send_chat_request(request).await?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(self.response_to_inference_result(ollama_response, duration_ms, &intent_by_tool))
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

        let prepared = crate::media::prepare_for_inference(
            context,
            self.model_has_vision(),
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
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

        let url = format!("{}/api/chat", self.host);
        // Retry the initial POST + status check (before any SSE event is
        // forwarded) so a transient upstream 5xx / network blip doesn't fail
        // the whole chat turn — matching the resilience of the non-streaming
        // path. `send_with_retry` returns the live `Response` with its body
        // stream intact on 2xx.
        let response = crate::retry::send_with_retry(
            "ollama",
            &self.retry_policy,
            &self.circuit_breaker,
            Some(&self.concurrency),
            || self.client.post(&url).json(&request),
        )
        .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                let reason = e.to_string();
                // Diagnostic: Ollama 400 on tool schema → dump tool list to /tmp
                // so we can pinpoint the malformed schema (often from MCP
                // servers). `send_with_retry` captures the response body in the
                // error reason, so match on that instead of the consumed body.
                if reason.contains("tool schema") || reason.contains("not of type") {
                    let dump = serde_json::json!({
                        "model": request.model,
                        "tool_count": request.tools.len(),
                        "tools": request.tools.iter().map(|t| serde_json::json!({
                            "name": t.function.name,
                            "parameters": t.function.parameters,
                        })).collect::<Vec<_>>(),
                    });
                    let path = format!(
                        "/tmp/agentos-ollama-bad-tools-{}.json",
                        chrono::Utc::now().timestamp()
                    );
                    if let Ok(s) = serde_json::to_string_pretty(&dump) {
                        let _ = std::fs::write(&path, s);
                        tracing::error!(path = %path, "Dumped offending Ollama tool list");
                    }
                }
                let reason = friendly_ollama_reason(reason);
                let _ = tx.send(InferenceEvent::Error(reason.clone())).await;
                return Err(AgentOSError::LLMError {
                    provider: "ollama".to_string(),
                    reason,
                });
            }
        };

        let mut full_text = String::new();
        let mut full_thinking = String::new();
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut done_reason: Option<String> = None;
        let mut tool_calls: Vec<InferenceToolCall> = Vec::new();

        const MAX_LINE_BUFFER_BYTES: usize = 1_048_576; // 1 MB
        let mut line_buf: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            line_buf.extend_from_slice(&chunk);

            if line_buf.len() > MAX_LINE_BUFFER_BYTES {
                let err = AgentOSError::LLMError {
                    provider: "ollama".to_string(),
                    reason: "NDJSON line buffer exceeded 1 MB".to_string(),
                };
                let _ = tx.send(InferenceEvent::Error(err.to_string())).await;
                return Err(err);
            }

            // Process complete NDJSON lines from the buffer.
            while let Some(newline_pos) = line_buf.iter().position(|&b| b == b'\n') {
                let line = &line_buf[..newline_pos];
                if !line.is_empty() {
                    if let Ok(resp) = serde_json::from_slice::<OllamaChatResponse>(line) {
                        if !resp.message.content.is_empty() {
                            full_text.push_str(&resp.message.content);
                            let _ = tx.send(InferenceEvent::Token(resp.message.content)).await;
                        }
                        // Accumulate thinking content from thinking models.
                        if let Some(ref thinking) = resp.message.thinking {
                            if !thinking.is_empty() {
                                full_thinking.push_str(thinking);
                            }
                        }
                        // Collect tool calls from ANY chunk — Ollama sends them
                        // in a separate done:false chunk before the final done:true.
                        // Guard: only collect once to prevent duplication.
                        if !resp.message.tool_calls.is_empty() && tool_calls.is_empty() {
                            for tc in &resp.message.tool_calls {
                                let payload = tool_helpers::validate_payload_object(
                                    &tc.function.name,
                                    "ollama",
                                    Some(tc.function.arguments.clone()),
                                );
                                if !tool_helpers::check_payload_size(&tc.function.name, &payload) {
                                    continue;
                                }
                                let itc = InferenceToolCall {
                                    id: None,
                                    tool_name: tc.function.name.clone(),
                                    intent_type: "execute".to_string(),
                                    payload,
                                };
                                let _ =
                                    tx.send(InferenceEvent::ToolCallComplete(itc.clone())).await;
                                tool_calls.push(itc);
                            }
                        }
                        if resp.done {
                            prompt_tokens = resp.prompt_eval_count.unwrap_or(0);
                            completion_tokens = resp.eval_count.unwrap_or(0);
                            done_reason = resp.done_reason;
                        }
                    }
                }
                line_buf.drain(..newline_pos + 1);
            }
        }

        // When content is empty but thinking has content and no tool calls,
        // use thinking as fallback (consistent with OpenAI/Custom adapters).
        if full_text.trim().is_empty() && !full_thinking.trim().is_empty() && tool_calls.is_empty()
        {
            tracing::info!(
                model = %self.model,
                thinking_len = full_thinking.len(),
                "Ollama stream content empty but thinking present — using thinking as fallback"
            );
            full_text = full_thinking;
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
        let prepared = crate::media::prepare_for_inference(
            context,
            self.model_has_vision(),
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
        let messages = self.context_to_messages(context);
        let ollama_tools: Vec<OllamaRequestTool> = tools
            .iter()
            .map(|t| OllamaRequestTool {
                tool_type: "function".to_string(),
                function: OllamaRequestToolFunction {
                    name: t.manifest.name.clone(),
                    description: t.manifest.description.clone(),
                    parameters: tool_helpers::normalize_tool_input_schema(
                        t.payload_schema.as_ref(),
                    ),
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

        let url = format!("{}/api/chat", self.host);
        // Retry the initial POST + status check (before any SSE event is
        // forwarded) so a transient upstream 5xx / network blip doesn't fail
        // the whole chat turn — matching the resilience of the non-streaming
        // path. `send_with_retry` returns the live `Response` with its body
        // stream intact on 2xx.
        let response = crate::retry::send_with_retry(
            "ollama",
            &self.retry_policy,
            &self.circuit_breaker,
            Some(&self.concurrency),
            || self.client.post(&url).json(&request),
        )
        .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                let reason = e.to_string();
                // Diagnostic: Ollama 400 on tool schema → dump tool list to /tmp
                // so we can pinpoint the malformed schema (often from MCP
                // servers). `send_with_retry` captures the response body in the
                // error reason, so match on that instead of the consumed body.
                if reason.contains("tool schema") || reason.contains("not of type") {
                    let dump = serde_json::json!({
                        "model": request.model,
                        "tool_count": request.tools.len(),
                        "tools": request.tools.iter().map(|t| serde_json::json!({
                            "name": t.function.name,
                            "parameters": t.function.parameters,
                        })).collect::<Vec<_>>(),
                    });
                    let path = format!(
                        "/tmp/agentos-ollama-bad-tools-{}.json",
                        chrono::Utc::now().timestamp()
                    );
                    if let Ok(s) = serde_json::to_string_pretty(&dump) {
                        let _ = std::fs::write(&path, s);
                        tracing::error!(path = %path, "Dumped offending Ollama tool list");
                    }
                }
                let reason = friendly_ollama_reason(reason);
                let _ = tx.send(InferenceEvent::Error(reason.clone())).await;
                return Err(AgentOSError::LLMError {
                    provider: "ollama".to_string(),
                    reason,
                });
            }
        };

        let mut full_text = String::new();
        let mut full_thinking = String::new();
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut done_reason: Option<String> = None;
        let mut tool_calls: Vec<InferenceToolCall> = Vec::new();

        const MAX_LINE_BUFFER_BYTES: usize = 1_048_576; // 1 MB
        let mut line_buf: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            line_buf.extend_from_slice(&chunk);

            if line_buf.len() > MAX_LINE_BUFFER_BYTES {
                let err = AgentOSError::LLMError {
                    provider: "ollama".to_string(),
                    reason: "NDJSON line buffer exceeded 1 MB".to_string(),
                };
                let _ = tx.send(InferenceEvent::Error(err.to_string())).await;
                return Err(err);
            }

            while let Some(newline_pos) = line_buf.iter().position(|&b| b == b'\n') {
                let line = &line_buf[..newline_pos];
                if !line.is_empty() {
                    if let Ok(resp) = serde_json::from_slice::<OllamaChatResponse>(line) {
                        if !resp.message.content.is_empty() {
                            full_text.push_str(&resp.message.content);
                            let _ = tx.send(InferenceEvent::Token(resp.message.content)).await;
                        }
                        // Accumulate thinking content from thinking models.
                        if let Some(ref thinking) = resp.message.thinking {
                            if !thinking.is_empty() {
                                full_thinking.push_str(thinking);
                            }
                        }
                        // Collect tool calls from ANY chunk — Ollama sends them
                        // in a separate done:false chunk before the final done:true.
                        // Guard: only collect once to prevent duplication if a
                        // future Ollama version repeats them in the done:true chunk.
                        if !resp.message.tool_calls.is_empty() && tool_calls.is_empty() {
                            for tc in &resp.message.tool_calls {
                                let payload = tool_helpers::validate_payload_object(
                                    &tc.function.name,
                                    "ollama",
                                    Some(tc.function.arguments.clone()),
                                );
                                if !tool_helpers::check_payload_size(&tc.function.name, &payload) {
                                    continue;
                                }
                                let itc = InferenceToolCall {
                                    id: None,
                                    tool_name: tc.function.name.clone(),
                                    intent_type: "execute".to_string(),
                                    payload,
                                };
                                let _ =
                                    tx.send(InferenceEvent::ToolCallComplete(itc.clone())).await;
                                tool_calls.push(itc);
                            }
                        }
                        if resp.done {
                            prompt_tokens = resp.prompt_eval_count.unwrap_or(0);
                            completion_tokens = resp.eval_count.unwrap_or(0);
                            done_reason = resp.done_reason;
                        }
                    }
                }
                line_buf.drain(..newline_pos + 1);
            }
        }

        // When content is empty but thinking has content and no tool calls
        // were made, use thinking as fallback text. Skipped when tool_calls
        // are present so internal reasoning doesn't leak alongside tool
        // execution (consistent with OpenAI/Custom adapters).
        if full_text.trim().is_empty() && !full_thinking.trim().is_empty() && tool_calls.is_empty()
        {
            tracing::info!(
                model = %self.model,
                thinking_len = full_thinking.len(),
                "Ollama stream content empty but thinking present — using thinking as fallback"
            );
            full_text = full_thinking;
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
    fn test_friendly_ollama_reason_subscription() {
        let reason = r#"API error 403 Forbidden: {"error":"this model requires a subscription, upgrade for access: https://ollama.com/upgrade"}"#;
        let msg = friendly_ollama_reason(reason.to_string());
        assert!(msg.contains("paid subscription"));
        assert!(msg.contains("ollama.com/upgrade"));
    }

    #[test]
    fn test_friendly_ollama_reason_unauth() {
        let msg =
            friendly_ollama_reason("API error 401 Unauthorized: {\"error\":\"bad token\"}".into());
        assert!(msg.contains("unauthenticated"));
    }

    #[test]
    fn test_friendly_ollama_reason_model_missing() {
        let reason =
            r#"API error 404 Not Found: {"error":"model 'foo' not found, try pulling it first"}"#;
        let msg = friendly_ollama_reason(reason.to_string());
        assert!(msg.contains("not installed"));
    }

    #[test]
    fn test_friendly_ollama_reason_falls_back_to_raw() {
        let reason = "API error 500 Internal Server Error: boom";
        let msg = friendly_ollama_reason(reason.to_string());
        // Unrecognised reasons are returned unchanged.
        assert_eq!(msg, reason);
    }

    #[test]
    fn test_friendly_ollama_reason_403_without_subscription_falls_back() {
        // 403 without a subscription / upgrade hint should not be rewritten —
        // we only want to translate the specific Ollama Cloud paywall pattern.
        let reason = "API error 403 Forbidden: {\"error\":\"forbidden\"}";
        let msg = friendly_ollama_reason(reason.to_string());
        assert_eq!(msg, reason);
    }

    #[test]
    fn test_friendly_ollama_reason_ignores_status_numbers_in_body() {
        // A 500 whose body merely mentions "401" / "404" must NOT be rewritten
        // as an auth / model-missing error — only the actual status segment
        // counts. Matching on a bare substring was the regression W1 fixed.
        let reason =
            r#"API error 500 Internal Server Error: {"error":"backend 404 for model qwen-401b"}"#;
        let msg = friendly_ollama_reason(reason.to_string());
        assert_eq!(msg, reason);
    }

    #[test]
    fn test_context_to_messages_conversion() {
        let mut ctx = ContextWindow::new(100);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            parts: vec![ContentPart::Text {
                text: "You are a helpful assistant.".into(),
            }],
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
            parts: vec![ContentPart::Text {
                text: "Hello!".into(),
            }],
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
            parts: vec![ContentPart::Text {
                text: "Say 'hello' and nothing else.".into(),
            }],
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
            parts: vec![ContentPart::Text {
                text: "tool output".to_string(),
            }],
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
            parts: vec![ContentPart::Text {
                text: "tool output".to_string(),
            }],
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

    #[test]
    fn test_ollama_vision_model_sets_images_array() {
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let mut ctx = ContextWindow::new(10);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![
                ContentPart::Text {
                    text: "describe".into(),
                },
                ContentPart::Image {
                    mime: "image/png".into(),
                    source: ImageSource::Base64 {
                        data: png_b64.into(),
                    },
                },
            ],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::Task,
            is_summary: false,
        });
        let adapter = OllamaCore::new("http://localhost:11434", "llava")
            .with_vision_models(vec!["llava".into()]);
        assert!(adapter.supports_images());
        let messages = adapter.context_to_messages(&ctx);
        assert_eq!(messages.len(), 1);
        let imgs = messages[0].images.as_ref().expect("images");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0], png_b64);
    }

    #[test]
    fn test_ollama_non_vision_model_strips_image_to_stub() {
        let mut ctx = ContextWindow::new(10);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![
                ContentPart::Text {
                    text: "x".into(),
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
            partition: ContextPartition::default(),
            category: ContextCategory::Task,
            is_summary: false,
        });
        let adapter = OllamaCore::new("http://localhost:11434", "llama3.2");
        assert!(!adapter.supports_images());
        let messages = adapter.context_to_messages(&ctx);
        assert!(messages[0].images.is_none());
        assert!(messages[0].content.contains("[Image:"));
    }

    #[test]
    fn test_ollama_message_deserializes_thinking_field() {
        let json = r#"{
            "model": "kimi-k2.5",
            "message": {
                "role": "assistant",
                "content": "",
                "thinking": "chain of thought here"
            },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 10,
            "eval_count": 30
        }"#;
        let resp: OllamaChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.message.thinking.as_deref(),
            Some("chain of thought here")
        );
        assert!(resp.message.content.is_empty());
    }

    #[test]
    fn test_ollama_message_without_thinking_field() {
        let json = r#"{
            "model": "llama3.2",
            "message": {
                "role": "assistant",
                "content": "Hello!"
            },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 5,
            "eval_count": 10
        }"#;
        let resp: OllamaChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.thinking.is_none());
        assert_eq!(resp.message.content, "Hello!");
    }

    #[test]
    fn test_ollama_thinking_fallback_when_content_empty() {
        let adapter = OllamaCore::new("http://localhost:11434", "kimi-k2.5");
        let json = r#"{
            "model": "kimi-k2.5",
            "message": {
                "role": "assistant",
                "content": "",
                "thinking": "The user asked a question and I should respond."
            },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 10,
            "eval_count": 30
        }"#;
        let resp: OllamaChatResponse = serde_json::from_str(json).unwrap();
        let result = adapter.response_to_inference_result(resp, 100, &HashMap::new());
        assert_eq!(
            result.text,
            "The user asked a question and I should respond."
        );
    }

    #[test]
    fn test_ollama_content_preferred_over_thinking() {
        let adapter = OllamaCore::new("http://localhost:11434", "kimi-k2.5");
        let json = r#"{
            "model": "kimi-k2.5",
            "message": {
                "role": "assistant",
                "content": "visible answer",
                "thinking": "internal reasoning"
            },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 10,
            "eval_count": 30
        }"#;
        let resp: OllamaChatResponse = serde_json::from_str(json).unwrap();
        let result = adapter.response_to_inference_result(resp, 100, &HashMap::new());
        assert_eq!(result.text, "visible answer");
    }

    #[test]
    fn test_ollama_tool_calls_deserialized_from_response() {
        let adapter = OllamaCore::new("http://localhost:11434", "kimi-k2.5");
        let json = r#"{
            "model": "kimi-k2.5",
            "message": {
                "role": "assistant",
                "content": "",
                "thinking": "I need to call file-writer",
                "tool_calls": [{
                    "function": {
                        "name": "file-writer",
                        "arguments": {"path": "test.txt", "content": "hello"}
                    }
                }]
            },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 10,
            "eval_count": 50
        }"#;
        let resp: OllamaChatResponse = serde_json::from_str(json).unwrap();
        let result = adapter.response_to_inference_result(resp, 100, &HashMap::new());
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].tool_name, "file-writer");
        assert_eq!(result.stop_reason, StopReason::ToolUse);
    }
}
