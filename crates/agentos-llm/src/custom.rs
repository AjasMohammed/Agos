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
    /// Non-streaming client. Bounded by the total request timeout only — see
    /// `build_http_client` for why no read timeout belongs here.
    client: Client,
    /// Streaming (SSE) client. Adds the read timeout, which is only meaningful
    /// when the response is supposed to arrive as a series of chunks.
    stream_client: Client,
    api_key: Option<SecretString>,
    model: String,
    base_url: String,
    capabilities: ModelCapabilities,
    pricing: ModelPricing,
    retry_policy: crate::retry::RetryPolicy,
    circuit_breaker: crate::retry::CircuitBreaker,
    /// In-flight cap for `base_url`, shared process-wide by every adapter
    /// pointed at it, so retries hold the slot and parallel chat sessions
    /// queue instead of stacking up on the same upstream rate-limit window.
    concurrency: Arc<tokio::sync::Semaphore>,
    image_resolver: Arc<dyn ImageResolver>,
    /// When non-empty, only these model names receive native image payloads.
    vision_models: Vec<String>,
    /// HTTP header used for auth ("Authorization", "api-key", ...).
    auth_header_name: String,
    /// Prefix prepended to the API key in the auth header ("Bearer ", "").
    auth_header_prefix: String,
    /// Chat completions path appended to base_url.
    chat_path: String,
    /// Models list path appended to base_url.
    models_path: String,
    /// Static extra headers appended to every request.
    extra_headers: Vec<(String, String)>,
    /// Explicit native tool-calling mode gate. Kept separate from generic tool
    /// support because many OpenAI-compatible hosts partially implement tools.
    native_tool_calling: bool,
    /// Total per-request timeout the `client` was built with. Doubles as the
    /// deadline for NVCF 202 status polling.
    request_timeout: std::time::Duration,
    /// JSON object merged into every request body (provider-specific knobs).
    extra_body: Option<Value>,
    /// Absolute URL template for polling async results, `{id}` substituted.
    /// Unset means "this provider never defers a result", so a 202 is left for
    /// the normal response path to deal with.
    status_url_template: Option<String>,
}

/// Max silence on the wire before a request fails.
const DEFAULT_READ_TIMEOUT_SECS: u64 = 60;
/// Total per-request timeout.
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = crate::traits::DEFAULT_INFERENCE_TIMEOUT_SECS;
/// Gap between async-result polls. NVCF's guidance is to poll immediately on
/// receiving the 202, then about once a second.
const NVCF_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// Consecutive poll failures tolerated before the generation is abandoned.
const MAX_NVCF_POLL_ERRORS: u32 = 3;

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
        // Hoisted: `base_url` is moved into the struct literal below.
        let concurrency = crate::retry::concurrency_limiter_for(&base_url);
        Self {
            client: Self::build_http_client(
                DEFAULT_REQUEST_TIMEOUT_SECS,
                DEFAULT_REQUEST_TIMEOUT_SECS,
            ),
            stream_client: Self::build_http_client(
                DEFAULT_READ_TIMEOUT_SECS,
                DEFAULT_REQUEST_TIMEOUT_SECS,
            ),
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
            concurrency,
            image_resolver: Arc::new(NoopImageResolver),
            vision_models: Vec::new(),
            auth_header_name: "Authorization".to_string(),
            auth_header_prefix: "Bearer ".to_string(),
            chat_path: "/chat/completions".to_string(),
            models_path: "/models".to_string(),
            extra_headers: Vec::new(),
            native_tool_calling: false,
            request_timeout: std::time::Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
            extra_body: None,
            status_url_template: None,
        }
    }

    /// `read_secs` fires after N seconds of silence on the wire, so it only
    /// says anything useful about a *streaming* response, where chunks are
    /// expected to keep arriving: it catches a hung server (e.g. the NVIDIA
    /// gateway flapping) long before `total_secs` would.
    ///
    /// A non-streaming `/chat/completions` call sends nothing at all until the
    /// model has finished generating, so on that path "silence on the wire" is
    /// the normal state and a read timeout is just a second, tighter total
    /// timeout wearing the wrong name — one that silently overrides the
    /// operator's `request_timeout_secs`. Build the non-stream client with
    /// `read_secs == total_secs` (see `new` / `with_catalog_overrides`).
    fn build_http_client(read_secs: u64, total_secs: u64) -> Client {
        Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(read_secs))
            .timeout(std::time::Duration::from_secs(total_secs))
            .build()
            .expect("HTTP client TLS initialization failed")
    }

    pub fn with_image_resolver(mut self, resolver: Arc<dyn ImageResolver>) -> Self {
        self.image_resolver = resolver;
        self
    }

    /// Override the HTTP auth header name and value prefix.
    /// Defaults: name `Authorization`, prefix `Bearer `.
    pub fn with_auth_scheme(
        mut self,
        header: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Self {
        self.auth_header_name = header.into();
        self.auth_header_prefix = prefix.into();
        self
    }

    /// Override the chat-completions and models endpoint paths.
    pub fn with_paths(
        mut self,
        chat_path: impl Into<String>,
        models_path: impl Into<String>,
    ) -> Self {
        self.chat_path = chat_path.into();
        self.models_path = models_path.into();
        self
    }

    /// Append a list of static headers to every request.
    pub fn with_extra_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Replace the model capabilities (context window, supports_*, etc.).
    pub fn with_capabilities(mut self, caps: ModelCapabilities) -> Self {
        self.capabilities = caps;
        self
    }

    /// Apply capability + auth + path overrides from a catalog entry, falling
    /// back to current values for any field the entry leaves unset.
    pub fn with_catalog_overrides(mut self, entry: &crate::catalog::CatalogEntry) -> Self {
        if let Some(v) = entry.context_window {
            self.capabilities.context_window_tokens = v;
        }
        if let Some(v) = entry.max_output_tokens {
            self.capabilities.max_output_tokens = v;
        }
        if let Some(v) = entry.supports_images {
            self.capabilities.supports_images = v;
        }
        if let Some(v) = entry.supports_tool_calling {
            self.capabilities.supports_tool_calling = v;
        }
        if let Some(v) = entry.supports_native_tool_calling {
            self.native_tool_calling = v;
        }
        if let Some(v) = entry.supports_streaming {
            self.capabilities.supports_streaming = v;
        }
        if let Some(v) = entry.supports_prompt_caching {
            self.capabilities.supports_prompt_caching = v;
        }
        if let Some(v) = entry.supports_json_mode {
            self.capabilities.supports_json_mode = v;
        }
        if let Some(v) = entry.supports_thinking {
            self.capabilities.supports_thinking = v;
        }
        if let Some(v) = &entry.auth_header {
            self.auth_header_name = v.clone();
        }
        if let Some(v) = &entry.auth_prefix {
            self.auth_header_prefix = v.clone();
        }
        if let Some(v) = &entry.chat_path {
            self.chat_path = v.clone();
        }
        if let Some(v) = &entry.models_path {
            self.models_path = v.clone();
        }
        if let Some(map) = &entry.extra_headers {
            self.extra_headers = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        if entry.read_timeout_secs.is_some() || entry.request_timeout_secs.is_some() {
            let read = entry.read_timeout_secs.unwrap_or(DEFAULT_READ_TIMEOUT_SECS);
            let total = entry
                .request_timeout_secs
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS);
            self.client = Self::build_http_client(total, total);
            self.stream_client = Self::build_http_client(read, total);
            self.request_timeout = std::time::Duration::from_secs(total);
        }
        if let Some(v) = &entry.status_url_template {
            self.status_url_template = Some(v.clone());
        }
        if let Some(raw) = &entry.extra_body_json {
            match serde_json::from_str::<Value>(raw) {
                Ok(v) if v.is_object() => self.extra_body = Some(v),
                Ok(_) => tracing::warn!(
                    provider = %entry.name,
                    "extra_body_json is not a JSON object — ignoring"
                ),
                Err(e) => tracing::warn!(
                    provider = %entry.name,
                    error = %e,
                    "extra_body_json is not valid JSON — ignoring"
                ),
            }
        }
        self
    }

    /// Merge the catalog's `extra_body` keys into a request body. Keys the
    /// adapter already set win, so a catalog knob can never clobber
    /// `model` / `messages` / `tools`.
    fn apply_extra_body(&self, body: &mut Value) {
        let Some(Value::Object(extra)) = self.extra_body.as_ref() else {
            return;
        };
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        for (k, v) in extra {
            obj.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }

    /// Request `max_tokens`: explicit option first, else the catalog's
    /// `max_output_tokens`. Sending it explicitly matters on hosts whose
    /// per-model default is far below the model's real cap (NVIDIA NIM
    /// defaults as low as 1024 on older models).
    ///
    /// Clamped to the context left over after the prompt: input and output
    /// share one window, so asking for the full output cap on top of a nearly
    /// full prompt is a 400 from most OpenAI-compatible hosts.
    fn apply_max_tokens(&self, body: &mut Value, requested: Option<u32>, estimated: u64) {
        let want = match requested {
            Some(t) => u64::from(t),
            None => self.capabilities.max_output_tokens,
        };
        if want == 0 {
            return;
        }
        let headroom = self
            .capabilities
            .context_window_tokens
            .saturating_sub(estimated);
        body["max_tokens"] = json!(want.min(headroom.max(1)));
    }

    /// NVIDIA's NIM gateway fronts models with NVIDIA Cloud Functions, which
    /// answers long generations with `202 Accepted` + an `NVCF-REQID` header
    /// and an empty body instead of a completion. The result must be polled
    /// until it returns 200 — a plain OpenAI client reads the 202 as a success
    /// with no `choices` and errors out.
    ///
    /// Returns `Ok(None)` when this is not a pollable NVCF 202 (wrong status,
    /// no request id, no configured `status_url_template`, or an id of
    /// unexpected shape), so the caller can handle the original response
    /// itself instead of mistaking "did nothing" for "resolved".
    async fn resolve_nvcf_202(
        &self,
        res: &reqwest::Response,
    ) -> Result<Option<reqwest::Response>, AgentOSError> {
        if res.status().as_u16() != 202 {
            return Ok(None);
        }
        let (Some(template), Some(req_id)) = (
            self.status_url_template.as_deref(),
            res.headers()
                .get("NVCF-REQID")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        ) else {
            return Ok(None);
        };
        // The id comes from an upstream response header and is spliced into a
        // URL that carries the API key, so anything but an opaque id (`../`,
        // `?`, `#`) is refused rather than sent.
        if req_id.is_empty()
            || !req_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            tracing::warn!(request_id = %req_id, "NVCF-REQID has unexpected shape — not polling");
            return Ok(None);
        }

        // No permit is taken here: `send_with_retry` now hands its permit back
        // to the caller, and both call sites hold it across this poll, so the
        // deferred generation is already inside the provider concurrency cap.
        // Grabbing a second one would double-count the same request.
        let url = template.replace("{id}", &req_id);
        let deadline = Instant::now() + self.request_timeout;
        let mut transient_errors = 0u32;
        // Info, not debug: a deferred generation is the difference between "the
        // kernel is idle" and "the provider is still working", and at DEBUG the
        // operator sees a quarter-hour of silence with no explanation.
        tracing::info!(
            request_id = %req_id,
            budget_secs = self.request_timeout.as_secs(),
            "Provider deferred the generation (202) — polling for the result"
        );
        let poll_started = Instant::now();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AgentOSError::LLMError {
                    provider: "custom".to_string(),
                    reason: format!(
                        "NVCF request {req_id} still pending after {}s",
                        self.request_timeout.as_secs()
                    ),
                });
            }

            // Bound each poll by the time left, otherwise a poll started just
            // before the deadline can still block for a full client timeout
            // and double the caller's advertised worst case.
            let sent =
                tokio::time::timeout(remaining, self.auth_header(self.client.get(&url)).send())
                    .await;

            let polled = match sent {
                Ok(Ok(p)) => {
                    transient_errors = 0;
                    p
                }
                // A generation minutes in is worth more than one failed GET.
                Ok(Err(e)) if transient_errors < MAX_NVCF_POLL_ERRORS => {
                    transient_errors += 1;
                    tracing::warn!(
                        request_id = %req_id,
                        error = %e,
                        transient_errors,
                        "NVCF status poll failed — retrying"
                    );
                    tokio::time::sleep(NVCF_POLL_INTERVAL).await;
                    continue;
                }
                Ok(Err(e)) => {
                    return Err(AgentOSError::LLMError {
                        provider: "custom".to_string(),
                        reason: format!("NVCF status poll failed: {e}"),
                    })
                }
                Err(_) => continue, // deadline elapsed; loop head reports it
            };

            match polled.status().as_u16() {
                200 => {
                    tracing::info!(
                        request_id = %req_id,
                        polled_secs = poll_started.elapsed().as_secs(),
                        "Deferred generation ready"
                    );
                    return Ok(Some(polled));
                }
                202 => tokio::time::sleep(NVCF_POLL_INTERVAL).await,
                s if crate::retry::is_retryable_status(s)
                    && transient_errors < MAX_NVCF_POLL_ERRORS =>
                {
                    transient_errors += 1;
                    tracing::warn!(
                        request_id = %req_id,
                        status = s,
                        transient_errors,
                        "NVCF status poll returned a retryable status"
                    );
                    tokio::time::sleep(NVCF_POLL_INTERVAL).await;
                }
                s => {
                    let body = polled.text().await.unwrap_or_default();
                    return Err(AgentOSError::LLMError {
                        provider: "custom".to_string(),
                        reason: format!("NVCF status poll HTTP {s}: {body}"),
                    });
                }
            }
        }
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
                    "parameters": tool_helpers::normalize_tool_input_schema(manifest.payload_schema.as_ref()),
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

                // Small/cloud models routinely emit tool calls inside a fenced
                // JSON block instead of native `tool_calls`. The recovery path
                // is the supported behaviour for them — log at debug so the
                // signal does not drown real warnings (every kimi/gemma/llama
                // turn that uses tools fires this branch).
                tracing::debug!(
                    tool = tool_name,
                    "Recovered tool call from fenced JSON in model text"
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

    /// Attach auth header if an API key is configured. Header name and prefix
    /// are configurable so providers like Azure (`api-key: <raw>`) work.
    fn auth_header(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req;
        if let Some(key) = &self.api_key {
            req = req.header(
                self.auth_header_name.as_str(),
                format!("{}{}", self.auth_header_prefix, key.expose_secret()),
            );
        }
        for (k, v) in &self.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        req
    }

    /// Hit the configured `models_path` and parse the response into model IDs.
    /// Accepts both OpenAI-style (`{"data": [{"id": "..."}, ...]}`) and Ollama-style
    /// (`{"models": [{"name": "..."}, ...]}`) shapes.
    pub async fn probe_models(&self) -> Result<Vec<String>, AgentOSError> {
        let url = self.endpoint_url(&self.models_path);
        let res = self
            .auth_header(self.client.get(&url))
            .send()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "custom".to_string(),
                reason: format!("Probe request failed: {e}"),
            })?;
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "custom".to_string(),
                reason: format!("Probe HTTP {status}: {body}"),
            });
        }
        let json: Value = res.json().await.map_err(|e| AgentOSError::LLMError {
            provider: "custom".to_string(),
            reason: format!("Probe response not JSON: {e}"),
        })?;
        let mut out = Vec::new();
        if let Some(arr) = json.get("data").and_then(Value::as_array) {
            for item in arr {
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    out.push(id.to_string());
                }
            }
        } else if let Some(arr) = json.get("models").and_then(Value::as_array) {
            for item in arr {
                if let Some(id) = item
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("name").and_then(Value::as_str))
                {
                    out.push(id.to_string());
                }
            }
        } else if let Some(arr) = json.as_array() {
            for item in arr {
                if let Some(id) = item.as_str() {
                    out.push(id.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Turn a non-streaming `/chat/completions` body into an `InferenceResult`.
    /// Shared by the plain non-stream path and the NVCF 202 poll result, which
    /// arrives as a complete JSON body even when streaming was requested.
    fn build_result_from_completion(
        &self,
        json_resp: &Value,
        intent_by_tool: &HashMap<String, String>,
        duration_ms: u64,
    ) -> Result<InferenceResult, AgentOSError> {
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
        let mut tool_calls = Self::parse_tool_calls(message, intent_by_tool);

        // Small-model fallback: some local models (e.g. gemma) emit tool
        // calls as fenced JSON in `content` rather than structured
        // `tool_calls`. Recover them so the kernel doesn't coherence-reject.
        if tool_calls.is_empty() && !text.is_empty() {
            let recovered = Self::parse_tool_calls_from_text(&text, intent_by_tool);
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

        let tokens_used = TokenUsage {
            prompt_tokens: json_resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            completion_tokens: json_resp["usage"]["completion_tokens"]
                .as_u64()
                .unwrap_or(0),
            total_tokens: json_resp["usage"]["total_tokens"].as_u64().unwrap_or(0),
        };
        let cached_tokens = json_resp["usage"]["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0);
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

    /// Compose `<base_url><path>` while tolerating either a trailing slash on
    /// the base URL or a leading slash on the path.
    fn endpoint_url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        if path.starts_with('/') {
            format!("{}{}", base, path)
        } else {
            format!("{}/{}", base, path)
        }
    }
}

#[async_trait]
impl LLMCore for CustomCore {
    fn supports_native_tool_calling(&self) -> bool {
        self.native_tool_calling && self.capabilities.supports_tool_calling
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
                provider: "custom".to_string(),
                reason: format!(
                    "Estimated token count ({estimated}) exceeds model context window ({max}). \
                     Reduce context or use a model with a larger window."
                ),
            });
        }

        let start_time = Instant::now();
        let url = self.endpoint_url(&self.chat_path);
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
        self.apply_max_tokens(&mut body, options.max_tokens, estimated);
        self.apply_extra_body(&mut body);

        // `_permit` holds the endpoint's concurrency slot until this scope
        // ends — which for this provider covers the NVCF 202 poll below, the
        // expensive half of a deferred generation.
        let (res, _permit) = crate::retry::send_with_retry(
            "custom",
            &self.retry_policy,
            &self.circuit_breaker,
            Some(&self.concurrency),
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
        let res = match self.resolve_nvcf_202(&res).await? {
            Some(polled) => polled,
            None => res,
        };

        let json_resp: Value = res.json().await.map_err(|e| AgentOSError::LLMError {
            provider: "custom".to_string(),
            reason: format!("Failed to parse JSON response: {}", e),
        })?;

        self.build_result_from_completion(
            &json_resp,
            &intent_by_tool,
            start_time.elapsed().as_millis() as u64,
        )
    }

    fn inference_hard_timeout_secs(&self) -> u64 {
        let budget = self.request_timeout.as_secs();
        if self.status_url_template.is_some() {
            // A deferred (NVCF 202) generation spends one budget on the initial
            // request and a second on the status poll, which `resolve_nvcf_202`
            // bounds separately. Allow for both or the ceiling would abort a
            // generation the adapter is still legitimately collecting.
            budget.saturating_mul(2)
        } else {
            budget
        }
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
        let url = self.endpoint_url(&self.models_path);
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
        let url = self.endpoint_url(&self.chat_path);
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
        self.apply_max_tokens(&mut body, None, estimated);
        self.apply_extra_body(&mut body);

        // Retry the initial POST + status check (before any SSE event is
        // forwarded) so a transient upstream 5xx / network blip doesn't fail
        // the whole chat turn. NVIDIA's NIM gateway intermittently returns
        // `500 unhashable type: 'dict'` on identical tool-calling payloads
        // (observed in kernel logs); a retry recovers it. `send_with_retry`
        // returns the live `Response` with its body stream intact on 2xx,
        // along with the endpoint concurrency permit. `_permit` is kept alive
        // for the whole of this function so the slot covers token generation
        // (and the NVCF poll): on a streamed request the headers arrive at the
        // *first* token, so releasing it here would leave chat — the busiest
        // caller — outside the cap entirely.
        let res = crate::retry::send_with_retry(
            "custom",
            &self.retry_policy,
            &self.circuit_breaker,
            Some(&self.concurrency),
            || {
                self.auth_header(
                    self.stream_client
                        .post(&url)
                        .header("Content-Type", "application/json")
                        .json(&body),
                )
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

        // NVCF answers long generations with 202 + a poll id and no SSE body
        // at all. Resolve it and replay the completed result as stream events.
        // A 202 that is *not* a pollable NVCF deferral yields `None` and falls
        // through to the normal SSE path below, which is what it was before.
        let deferred = match self.resolve_nvcf_202(&res).await {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(InferenceEvent::Error(e.to_string())).await;
                return Err(e);
            }
        };
        if let Some(polled) = deferred {
            let outcome = async {
                let json_resp: Value = polled.json().await.map_err(|e| AgentOSError::LLMError {
                    provider: "custom".to_string(),
                    reason: format!("Failed to parse NVCF poll response: {e}"),
                })?;
                self.build_result_from_completion(
                    &json_resp,
                    &intent_by_tool,
                    start_time.elapsed().as_millis() as u64,
                )
            }
            .await;
            let result = match outcome {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(InferenceEvent::Error(e.to_string())).await;
                    return Err(e);
                }
            };
            // No `Token` event on purpose: the whole answer arrived at once, so
            // emitting it as a single token would paint it in one jump. Sending
            // none lets the kernel's fallback chunker replay `Done.text`
            // progressively, the same as for any non-streaming adapter.
            for tc in &result.tool_calls {
                let _ = tx.send(InferenceEvent::ToolCallComplete(tc.clone())).await;
            }
            let _ = tx
                .send(InferenceEvent::Usage(result.tokens_used.clone()))
                .await;
            let _ = tx.send(InferenceEvent::Done(result)).await;
            return Ok(());
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
        // Diagnostics for a stream that yields nothing usable (see the
        // empty-stream guard after the loop).
        let mut non_sse_lines: u32 = 0;
        let mut unparsed_lines: u32 = 0;
        let mut first_offending_line = String::new();

        const MAX_LINE_BUFFER_BYTES: usize = 1_048_576; // 1 MB

        // Carry buffer for a multibyte UTF-8 sequence split across HTTP chunks.
        let mut utf8_pending: Vec<u8> = Vec::new();
        let mut stream = res.bytes_stream();
        'outer: while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "custom".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            crate::streaming_helpers::push_utf8_chunk(&mut utf8_pending, &chunk, &mut line_buffer);

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
                // The SSE spec makes the space after `data:` optional; some
                // OpenAI-compatible servers omit it.
                let data = if let Some(d) = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
                {
                    d.trim()
                } else {
                    non_sse_lines += 1;
                    if first_offending_line.is_empty() {
                        first_offending_line = line.chars().take(300).collect();
                    }
                    continue;
                };
                if data == "[DONE]" {
                    break 'outer;
                }
                let Ok(chunk_json) = serde_json::from_str::<Value>(data) else {
                    unparsed_lines += 1;
                    if first_offending_line.is_empty() {
                        first_offending_line = data.chars().take(300).collect();
                    }
                    continue;
                };

                // Mid-stream provider failure: NVIDIA NIM / vLLM / LiteLLM
                // report these as an `error` payload on a 200 response. Without
                // this the stream just ends and the caller gets a blank answer.
                if let Some(err) = chunk_json.get("error").filter(|e| !e.is_null()) {
                    let reason = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| err.to_string());
                    let _ = tx.send(InferenceEvent::Error(reason.clone())).await;
                    return Err(AgentOSError::LLMError {
                        provider: "custom".to_string(),
                        reason,
                    });
                }

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

        // A 200 whose stream carried no text, no tool call and no usage is not
        // a completion — it is a truncated or silently rejected request. Fail
        // loudly; the caller otherwise renders an empty assistant turn.
        if full_text.trim().is_empty() && tool_calls.is_empty() && usage.total_tokens == 0 {
            let reason = format!(
                "provider stream ended with no content (stop_reason={:?}, non_sse_lines={}, \
                 unparsed_lines={}, first_offending_line={})",
                stop_reason,
                non_sse_lines,
                unparsed_lines,
                if first_offending_line.is_empty() {
                    "<none>"
                } else {
                    first_offending_line.as_str()
                }
            );
            tracing::warn!(model = %self.model, %reason, "Custom stream produced no content");
            let _ = tx.send(InferenceEvent::Error(reason.clone())).await;
            return Err(AgentOSError::LLMError {
                provider: "custom".to_string(),
                reason,
            });
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

    #[test]
    fn test_endpoint_url_default_paths() {
        let adapter = CustomCore::new(
            None,
            "m".to_string(),
            "https://api.example.com/v1".to_string(),
        );
        assert_eq!(
            adapter.endpoint_url(&adapter.chat_path),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            adapter.endpoint_url(&adapter.models_path),
            "https://api.example.com/v1/models"
        );
    }

    #[test]
    fn test_endpoint_url_trailing_slash_on_base() {
        let adapter = CustomCore::new(
            None,
            "m".to_string(),
            "https://api.example.com/v1/".to_string(),
        );
        // No double slash.
        assert_eq!(
            adapter.endpoint_url("/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_endpoint_url_path_without_leading_slash() {
        let adapter = CustomCore::new(
            None,
            "m".to_string(),
            "https://api.example.com/v1".to_string(),
        );
        assert_eq!(
            adapter.endpoint_url("models"),
            "https://api.example.com/v1/models"
        );
    }

    #[test]
    fn test_endpoint_url_azure_style_with_query_string() {
        let adapter = CustomCore::new(
            None,
            "gpt-4o".to_string(),
            "https://r.openai.azure.com/openai".to_string(),
        )
        .with_paths(
            "/deployments/gpt-4o/chat/completions?api-version=2024-08-01-preview",
            "/models?api-version=2024-08-01-preview",
        );
        assert_eq!(
            adapter.endpoint_url(&adapter.chat_path),
            "https://r.openai.azure.com/openai/deployments/gpt-4o/chat/completions?api-version=2024-08-01-preview"
        );
    }

    #[test]
    fn test_with_catalog_overrides_apply_capability_fields() {
        use crate::catalog::CatalogEntry;
        let adapter = CustomCore::new(None, "m".to_string(), "https://api.example.com".to_string());
        let entry = CatalogEntry {
            name: "x".into(),
            display_name: "X".into(),
            base_url: "https://api.example.com".into(),
            api_key_env: String::new(),
            compatible_with: "openai".into(),
            default_model: "m".into(),
            context_window: Some(128_000),
            supports_images: Some(true),
            supports_tool_calling: Some(false),
            supports_native_tool_calling: Some(true),
            supports_prompt_caching: Some(true),
            auth_header: Some("api-key".into()),
            auth_prefix: Some(String::new()),
            chat_path: Some("/v2/chat".into()),
            models_path: Some("/v2/models".into()),
            ..Default::default()
        };
        let adapter = adapter.with_catalog_overrides(&entry);
        assert_eq!(adapter.capabilities.context_window_tokens, 128_000);
        assert!(adapter.capabilities.supports_images);
        assert!(!adapter.capabilities.supports_tool_calling);
        // Native mode must remain off when tool-calling itself is disabled.
        assert!(!adapter.supports_native_tool_calling());
        assert!(adapter.capabilities.supports_prompt_caching);
        assert_eq!(adapter.auth_header_name, "api-key");
        assert_eq!(adapter.auth_header_prefix, "");
        assert_eq!(adapter.chat_path, "/v2/chat");
        assert_eq!(adapter.models_path, "/v2/models");
    }

    #[test]
    fn test_extra_body_merges_without_clobbering_adapter_keys() {
        use crate::catalog::CatalogEntry;
        let adapter = CustomCore::new(None, "m".to_string(), "https://api.example.com".to_string())
            .with_catalog_overrides(&CatalogEntry {
                extra_body_json: Some(
                    r#"{"chat_template_kwargs":{"enable_thinking":false},"model":"hijacked"}"#
                        .into(),
                ),
                ..Default::default()
            });
        let mut body = json!({"model": "m", "messages": []});
        adapter.apply_extra_body(&mut body);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        // Adapter-set keys win.
        assert_eq!(body["model"], "m");
    }

    #[test]
    fn test_invalid_extra_body_json_is_ignored() {
        use crate::catalog::CatalogEntry;
        for raw in [r#"{"a":"#, r#""not an object""#] {
            let adapter =
                CustomCore::new(None, "m".to_string(), "https://api.example.com".to_string())
                    .with_catalog_overrides(&CatalogEntry {
                        extra_body_json: Some(raw.into()),
                        ..Default::default()
                    });
            assert!(adapter.extra_body.is_none(), "{raw}");
            let mut body = json!({"model": "m"});
            adapter.apply_extra_body(&mut body);
            assert_eq!(body, json!({"model": "m"}));
        }
    }

    #[test]
    fn test_max_tokens_falls_back_to_catalog_cap() {
        use crate::catalog::CatalogEntry;
        let adapter = CustomCore::new(None, "m".to_string(), "https://api.example.com".to_string())
            .with_catalog_overrides(&CatalogEntry {
                max_output_tokens: Some(8192),
                context_window: Some(128_000),
                ..Default::default()
            });

        // No explicit request → catalog cap is sent so the host does not apply
        // its own (much lower) per-model default.
        let mut body = json!({});
        adapter.apply_max_tokens(&mut body, None, 1_000);
        assert_eq!(body["max_tokens"], 8192);

        // Explicit request wins.
        let mut body = json!({});
        adapter.apply_max_tokens(&mut body, Some(512), 1_000);
        assert_eq!(body["max_tokens"], 512);

        // Input and output share the window, so the cap is clamped to what the
        // prompt left behind — otherwise the host 400s on a near-full context.
        let mut body = json!({});
        adapter.apply_max_tokens(&mut body, None, 125_000);
        assert_eq!(body["max_tokens"], 3_000);

        // Never emit a non-positive cap, even when the estimate fills the
        // window completely.
        let mut body = json!({});
        adapter.apply_max_tokens(&mut body, None, 999_999);
        assert_eq!(body["max_tokens"], 1);

        // No cap configured → key omitted entirely.
        let bare = CustomCore::new(None, "m".to_string(), "https://api.example.com".to_string());
        let mut body = json!({});
        bare.apply_max_tokens(&mut body, None, 10);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn test_timeout_overrides_apply_from_catalog() {
        use crate::catalog::CatalogEntry;
        let adapter = CustomCore::new(None, "m".to_string(), "https://api.example.com".to_string());
        assert_eq!(
            adapter.request_timeout,
            std::time::Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS)
        );
        let adapter = adapter.with_catalog_overrides(&CatalogEntry {
            request_timeout_secs: Some(600),
            read_timeout_secs: Some(300),
            ..Default::default()
        });
        assert_eq!(adapter.request_timeout, std::time::Duration::from_secs(600));
        // No deferred-result polling configured: the ceiling is the budget.
        assert_eq!(adapter.inference_hard_timeout_secs(), 600);

        let deferred = adapter.with_catalog_overrides(&CatalogEntry {
            request_timeout_secs: Some(600),
            read_timeout_secs: Some(300),
            status_url_template: Some("https://poll.example.com/{id}".to_string()),
            ..Default::default()
        });
        // Deferred generations spend one budget on the request and one on the
        // poll, so the ceiling has to cover both.
        assert_eq!(deferred.inference_hard_timeout_secs(), 1200);
    }

    /// `request_timeout_secs` is the only hard deadline on a non-streaming
    /// completion, so it has to actually fire against a server that accepts the
    /// connection and then never answers.
    #[tokio::test]
    async fn request_timeout_is_a_real_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock); // accept, never respond
            }
        });

        let client = CustomCore::build_http_client(3, 3);
        let started = std::time::Instant::now();
        let res = client.get(format!("http://{addr}/x")).send().await;
        assert!(res.is_err(), "stalled request must not hang forever");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(8),
            "timeout fired after {:?}",
            started.elapsed()
        );
    }

    /// A non-streaming completion sends nothing at all until the model has
    /// finished, so the read timeout must not bound it — otherwise
    /// `read_timeout_secs` silently overrides the operator's
    /// `request_timeout_secs`. NVIDIA NIM's 300 beat its own 600 that way and
    /// killed healthy reasoning turns at exactly 300s.
    #[tokio::test]
    async fn read_timeout_bounds_only_the_streaming_client() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    // Silence for longer than the read timeout, well inside the
                    // total budget — exactly what a slow reasoning model looks
                    // like on the wire.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .await;
                    let _ = sock.flush().await;
                });
            }
        });

        let adapter = CustomCore::new(None, "m".to_string(), format!("http://{addr}"))
            .with_catalog_overrides(&crate::catalog::CatalogEntry {
                read_timeout_secs: Some(1),
                request_timeout_secs: Some(30),
                ..Default::default()
            });

        let url = format!("http://{addr}/v1/chat/completions");
        assert!(
            adapter.client.get(&url).send().await.is_ok(),
            "non-stream client must be bounded by request_timeout_secs only"
        );
        assert!(
            adapter.stream_client.get(&url).send().await.is_err(),
            "stream client must still fail fast on an inter-chunk stall"
        );
    }

    /// Serve one canned HTTP response on a throwaway port and return its base
    /// URL. Used to drive the SSE parser against pathological provider bodies.
    async fn serve_once(body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    async fn stream_err(body: &'static str) -> String {
        let base = serve_once(body).await;
        let adapter = CustomCore::new(None, "m".to_string(), base);
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![ContentPart::Text {
                text: "hi".to_string(),
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
        let (tx, _rx) = mpsc::channel(64);
        match adapter.infer_stream_with_tools(&ctx, &[], tx).await {
            Ok(()) => panic!("expected an error, got a successful empty completion"),
            Err(e) => e.to_string(),
        }
    }

    /// A 200 response whose SSE body carries an `error` payload must fail, not
    /// resolve to an empty assistant turn.
    #[tokio::test]
    async fn stream_error_payload_is_surfaced() {
        let msg = stream_err(
            "data: {\"error\":{\"message\":\"upstream overloaded\"}}\n\ndata: [DONE]\n\n",
        )
        .await;
        assert!(msg.contains("upstream overloaded"), "got: {msg}");
    }

    /// A stream that ends with no text, no tool call and no usage is a
    /// truncated request, not a completion.
    #[tokio::test]
    async fn empty_stream_is_an_error() {
        let msg = stream_err("data: [DONE]\n\n").await;
        assert!(msg.contains("no content"), "got: {msg}");
    }

    #[test]
    fn test_custom_native_tool_calling_is_explicit_opt_in() {
        use crate::catalog::CatalogEntry;
        let base = CustomCore::new(None, "m".to_string(), "https://api.example.com".to_string());
        assert!(!base.supports_native_tool_calling());

        let enabled = base.with_catalog_overrides(&CatalogEntry {
            name: "x".into(),
            display_name: "X".into(),
            base_url: "https://api.example.com".into(),
            api_key_env: String::new(),
            compatible_with: "openai".into(),
            default_model: "m".into(),
            supports_tool_calling: Some(true),
            supports_native_tool_calling: Some(true),
            ..Default::default()
        });
        assert!(enabled.supports_native_tool_calling());
    }
}
