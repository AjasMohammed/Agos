use crate::media::{anthropic_blocks_for_entry, ImageResolver, NoopImageResolver};
use crate::tool_helpers;
use crate::traits::LLMCore;
use crate::types::{
    calculate_inference_cost, default_pricing_table, InferenceEvent, InferenceOptions,
    InferenceResult, InferenceToolCall, ModelCapabilities, ModelPricing, PromptCacheTtl,
    StopReason, TokenUsage, ToolChoice,
};
use agentos_types::*;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    /// In-flight cap for outbound requests, shared process-wide by every
    /// adapter pointed at the same `base_url`.
    concurrency: Arc<tokio::sync::Semaphore>,
    image_resolver: Arc<dyn ImageResolver>,
    /// Set once the API rejects `defer_loading`/`tool_reference` (400): this
    /// adapter instance then sends every tool inline (no deferral).
    deferral_rejected: AtomicBool,
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
        // Hoisted: `base_url` is moved into the struct literal below.
        let concurrency = crate::retry::concurrency_limiter_for(&base_url);
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
            concurrency,
            image_resolver: Arc::new(NoopImageResolver),
            deferral_rejected: AtomicBool::new(false),
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

    /// File-backed image resolution for `ImageSource::FileRef` (e.g. web uploads).
    pub fn with_image_resolver(mut self, resolver: Arc<dyn ImageResolver>) -> Self {
        self.image_resolver = resolver;
        self
    }

    /// `emit_refs`: render `tool_reference` blocks from `context.tool_references`.
    /// Only valid when this request sends the full catalogue with
    /// `defer_loading` (native deferral active, tools non-empty); otherwise a
    /// reference names a tool absent from `tools` and the API returns 400.
    fn format_messages(&self, context: &ContextWindow, emit_refs: bool) -> Vec<serde_json::Value> {
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
                        // Provider-native deferral: a discovery result carries
                        // `tool_reference` blocks for the deferred tools it
                        // surfaced; the API expands them after the cached prefix.
                        let content =
                            match context
                                .tool_references
                                .get(use_id)
                                .filter(|names| emit_refs && !names.is_empty())
                            {
                                Some(names) => {
                                    let mut blocks =
                                        vec![json!({"type": "text", "text": entry.text()})];
                                    blocks.extend(names.iter().map(
                                        |n| json!({"type": "tool_reference", "tool_name": n}),
                                    ));
                                    Value::Array(blocks)
                                }
                                None => json!(entry.text()),
                            };
                        pending_tool_results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": use_id,
                            "content": content,
                        }));
                    } else {
                        // Legacy fallback: add as a text content block in the
                        // pending batch to avoid consecutive user messages.
                        pending_tool_results.push(json!({
                            "type": "text",
                            "text": format!("Tool Result:\n{}", entry.text()),
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
                            let blocks = anthropic_blocks_for_entry(
                                entry,
                                self.capabilities.supports_images,
                                &self.image_resolver,
                            )
                            .unwrap_or_else(|e| {
                                vec![json!({
                                    "type": "text",
                                    "text": format!("[[multimodal error: {e}]]"),
                                })]
                            });
                            messages.push(json!({
                                "role": "user",
                                "content": Value::Array(blocks),
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
                                if !entry.text().is_empty() {
                                    content_blocks
                                        .push(json!({"type": "text", "text": entry.text()}));
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
                                    "content": entry.text(),
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

    /// `deferred_from`: tools at index ≥ n carry `defer_loading: true` (they stay
    /// out of the cached prefix until Claude discovers them via a
    /// `tool_reference`). `None` = every tool loaded.
    fn build_anthropic_tools(
        tools: &[ToolManifest],
        deferred_from: Option<usize>,
    ) -> (Vec<Value>, HashMap<String, String>, Vec<bool>) {
        let mut anthropic_tools = Vec::new();
        let mut intent_by_tool = HashMap::new();
        let mut seen_names = HashSet::new();
        // Which input positions survived dedup: the kernel's prefix/deferral
        // indices are input positions and must be mapped to output positions
        // before placing the cache breakpoint (see `kept_index`).
        let mut kept = vec![false; tools.len()];

        for (idx, manifest) in tools.iter().enumerate() {
            let tool_name = manifest.manifest.name.trim();
            if tool_name.is_empty() || !seen_names.insert(tool_name.to_string()) {
                continue;
            }
            kept[idx] = true;

            let intent_type = tool_helpers::infer_intent_type_from_permissions(
                &manifest.capabilities_required.permissions,
            );
            intent_by_tool.insert(tool_name.to_string(), intent_type);

            let mut tool = json!({
                "name": tool_name,
                "description": manifest.manifest.description,
                // Anthropic Messages API requires `input_schema` (NOT `payload_schema`).
                // Using the wrong key causes 400 errors or silent schemaless tool definitions.
                // Examples embedded inside input_schema via JSON-Schema "examples" keyword
                // — survives the Anthropic API unchanged (sibling keys would 400).
                "input_schema": tool_helpers::normalize_tool_input_schema_with_examples(manifest.payload_schema.as_ref(), &manifest.examples),
            });
            if deferred_from.is_some_and(|n| idx >= n) {
                tool["defer_loading"] = json!(true);
            }
            anthropic_tools.push(tool);
        }

        (anthropic_tools, intent_by_tool, kept)
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

/// Cache marker for the requested TTL. The 5m form omits `ttl` so request
/// bodies stay byte-identical to the legacy behavior; 1h requires the
/// `extended-cache-ttl-2025-04-11` beta header on the request.
fn cache_control_value(ttl: PromptCacheTtl) -> Value {
    match ttl {
        PromptCacheTtl::FiveMinutes => json!({ "type": "ephemeral" }),
        PromptCacheTtl::OneHour => json!({ "type": "ephemeral", "ttl": "1h" }),
    }
}

/// Build a cost-weighted `TokenUsage` for pricing. Anthropic bills cache WRITES
/// at 1.25x and cache READS at 0.1x the base input rate, and `input_tokens`
/// excludes both. Folding the weighted cache tokens into `prompt_tokens` lets
/// the flat-rate `calculate_inference_cost` produce the true billed amount
/// without a schema change across every provider's `TokenUsage` literal (H2).
/// This value is for COST ONLY — the literal `tokens_used` on the result keeps
/// the real, unweighted counts.
fn weighted_usage_for_cost(
    input_tokens: u64,
    output_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
) -> TokenUsage {
    let billable_input = input_tokens
        + (cache_write_tokens as f64 * 1.25).round() as u64
        + (cache_read_tokens as f64 * 0.10).round() as u64;
    TokenUsage {
        prompt_tokens: billable_input,
        completion_tokens: output_tokens,
        total_tokens: billable_input + output_tokens,
    }
}

/// Attach `cache_control: {type: ephemeral}` to the *last* content block of a
/// message envelope. Anthropic accepts string or array `content`. For string
/// content the message is rewritten as a single-element text array so the
/// cache marker can ride on it.
fn attach_cache_control_to_last_block(message: &mut Value, ttl: PromptCacheTtl) {
    let Some(obj) = message.as_object_mut() else {
        return;
    };
    match obj.get_mut("content") {
        Some(Value::Array(blocks)) => {
            if let Some(last) = blocks.last_mut() {
                if let Some(block_obj) = last.as_object_mut() {
                    block_obj.insert("cache_control".into(), cache_control_value(ttl));
                }
            }
        }
        Some(Value::String(s)) => {
            let text = std::mem::take(s);
            let mut block = json!({
                "type": "text",
                "text": text,
            });
            block["cache_control"] = cache_control_value(ttl);
            obj.insert("content".into(), json!([block]));
        }
        _ => {}
    }
}

#[async_trait]
impl LLMCore for AnthropicCore {
    fn supports_native_tool_calling(&self) -> bool {
        true
    }

    fn supports_deferred_tools(&self) -> bool {
        model_supports_deferred_tools(&self.model)
            && !self.deferral_rejected.load(Ordering::Relaxed)
    }

    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        self.infer_with_tools(context, &[]).await
    }

    async fn infer_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
    ) -> Result<InferenceResult, AgentOSError> {
        // Caching is always on for Anthropic — matches the streaming path and
        // the kernel's task-path policy. `InferenceOptions::default()` has
        // `enable_prompt_caching: false`, which silently disabled caching on
        // every non-streaming chat turn.
        self.infer_with_options(
            context,
            tools,
            &InferenceOptions {
                enable_prompt_caching: true,
                ..InferenceOptions::default()
            },
        )
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

        let prepared = crate::media::prepare_for_inference(
            context,
            self.capabilities.supports_images,
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
        // If options disable tools, exclude them from the request.
        let effective_tools = if matches!(options.tool_choice, Some(ToolChoice::None)) {
            &[][..]
        } else {
            tools
        };
        // Guard at the point of use: the kernel decides deferral per iteration,
        // but the adapter may have flipped `deferral_rejected` (400) since.
        // Then every tool goes inline (expensive, not fatal) and no
        // `tool_reference` may be rendered: it would name a tool absent from
        // `tools`.
        let deferred_from = options
            .deferred_tools_from
            .filter(|_| self.supports_deferred_tools() && !effective_tools.is_empty());
        let mut messages = self.format_messages(context, deferred_from.is_some());
        let active = context.active_entries();
        let image_count = active
            .iter()
            .flat_map(|e| e.parts.iter())
            .filter(|p| matches!(p, ContentPart::Image { .. }))
            .count();
        let system_entries: Vec<&ContextEntry> = active
            .iter()
            .filter(|e| e.role == ContextRole::System)
            .copied()
            .collect();
        let system_prompt = system_entries.first().map(|e| e.text()).unwrap_or_default();

        // Extended thinking requires max_tokens > budget_tokens because both
        // thinking tokens and response tokens count against max_tokens.
        // We ensure at least 4 096 response-token headroom beyond the thinking budget.
        let mut max_tokens = options.max_tokens.unwrap_or(self.max_tokens);
        if let Some(budget) = options.thinking_budget_tokens {
            let minimum = budget.saturating_add(4_096);
            if max_tokens < minimum {
                max_tokens = minimum;
            }
        }

        let (anthropic_tools, intent_by_tool, kept) =
            Self::build_anthropic_tools(effective_tools, deferred_from);
        let cache_prefix = deferral_cache_prefix(
            kept_index(&kept, options.tools_cache_prefix_len),
            kept_index(&kept, deferred_from),
        );

        // Prompt caching: represent the system prompt as multiple blocks and place
        // a cache breakpoint at the tools/manual block. This keeps the stable prefix
        // cached while allowing later dynamic system blocks to vary.
        let system_value = if options.enable_prompt_caching && !system_entries.is_empty() {
            let mut blocks: Vec<Value> = Vec::new();
            let mut breakpoint_set = false;
            for entry in &system_entries {
                let mut block = json!({
                    "type": "text",
                    "text": entry.text(),
                });
                if !breakpoint_set && entry.category == ContextCategory::Tools {
                    block["cache_control"] = cache_control_value(options.cache_ttl);
                    breakpoint_set = true;
                }
                blocks.push(block);
            }
            if !breakpoint_set && !blocks.is_empty() {
                blocks[0]["cache_control"] = cache_control_value(options.cache_ttl);
            }
            Value::Array(blocks)
        } else if !system_entries.is_empty() {
            let merged = system_entries
                .iter()
                .map(|e| e.text())
                .collect::<Vec<_>>()
                .join("\n\n");
            json!(merged)
        } else {
            json!(system_prompt)
        };

        // Prompt caching breakpoint #3: stable conversation prefix. Mark the
        // *next-to-last* message so the prior turn's content hits cache on the
        // following request, while the new turn (which inevitably differs) is
        // free to grow without invalidating the cache. Anthropic supports up
        // to 4 breakpoints; we use #1 system, #2 tools (set below), #3 here.
        if options.enable_prompt_caching && messages.len() >= 2 {
            let idx = messages.len() - 2;
            attach_cache_control_to_last_block(&mut messages[idx], options.cache_ttl);
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "system": system_value,
            "messages": messages,
        });

        // Extended thinking: inject the thinking block and enable the beta feature.
        // Only supported on claude-3-7-sonnet and newer models.
        // NOTE: When thinking is enabled temperature must be 1.0 (Anthropic requirement).
        if let Some(budget) = options.thinking_budget_tokens {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget
            });
        }

        if !anthropic_tools.is_empty() {
            // Prompt caching breakpoint #2: tools array. Marking the last tool
            // definition with `cache_control` caches the entire tools block as a
            // unit. Whenever the tool registry changes the cache invalidates;
            // otherwise every multi-turn request reads tools from cache.
            let mut tools_array = anthropic_tools;
            if options.enable_prompt_caching {
                if let Some(t) = cache_breakpoint_tool(&mut tools_array, cache_prefix) {
                    t["cache_control"] = cache_control_value(options.cache_ttl);
                }
            }
            body["tools"] = Value::Array(tools_array);
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
            // Anthropic rejects any temperature != 1.0 when extended thinking is enabled.
            // Skip non-1.0 values silently rather than erroring; the thinking block is
            // more important than a temperature override.
            let thinking_active = options.thinking_budget_tokens.is_some();
            if !thinking_active || (temp - 1.0_f32).abs() < f32::EPSILON {
                body["temperature"] = json!(temp);
            }
        }

        tracing::debug!(
            target: "agentos::llm::input",
            model = %self.model,
            image_count,
            "LLM input body: {}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string())
        );

        let thinking_enabled = options.thinking_budget_tokens.is_some();
        // Beta features ride a single comma-joined `anthropic-beta` header.
        let mut beta_features: Vec<&str> = Vec::new();
        if thinking_enabled {
            // Extended thinking requires the interleaved-thinking beta header.
            beta_features.push("interleaved-thinking-2025-05-14");
        }
        if options.enable_prompt_caching && options.cache_ttl == PromptCacheTtl::OneHour {
            beta_features.push("extended-cache-ttl-2025-04-11");
        }
        let beta_header = (!beta_features.is_empty()).then(|| beta_features.join(","));
        // `_permit` holds the endpoint's concurrency slot until this scope
        // ends, i.e. until the (non-streamed) body has been read.
        let (res, _permit) = crate::retry::send_with_retry(
            "anthropic",
            &self.retry_policy,
            &self.circuit_breaker,
            Some(&self.concurrency),
            || {
                let mut req = self
                    .client
                    .post(&url)
                    .header("x-api-key", self.api_key.expose_secret())
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json");
                if let Some(ref beta) = beta_header {
                    req = req.header("anthropic-beta", beta.as_str());
                }
                req.json(&body)
            },
        )
        .await
        .inspect_err(|e| self.note_deferral_rejection(e, deferred_from.is_some()))?;

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
        let cache_write_tokens = json_resp["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);

        let tokens_used = TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        };
        // Anthropic's `input_tokens` EXCLUDES cached tokens; cache writes are
        // billed at 1.25x and cache reads at 0.1x the base input rate. Bill cost
        // on a weighted input count so budget enforcement reflects true spend —
        // otherwise a long cached task's tracked cost is a fraction of reality
        // and the hard-limit suspend fires far too late (H2). `tokens_used`
        // stays literal for accurate token reporting.
        let cost = calculate_inference_cost(
            &weighted_usage_for_cost(
                prompt_tokens,
                completion_tokens,
                cache_write_tokens,
                cached_tokens,
            ),
            &self.pricing,
        );

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
        // Use the non-billable GET /models endpoint instead of POST /messages.
        // A real inference (even max_tokens=1) is billed on every probe, and
        // health checks run periodically — a recurring charge just for liveness.
        // GET /models validates reachability + API key auth at zero token cost.
        let url = format!("{}/models", self.base_url);

        match self
            .client
            .get(&url)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", "2023-06-01")
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

        let prepared = crate::media::prepare_for_inference(
            context,
            self.capabilities.supports_images,
            self.image_resolver.clone(),
            &self.client,
        )
        .await;
        let context = &prepared;
        let mut messages = self.format_messages(context, false);
        let active = context.active_entries();
        let image_count = active
            .iter()
            .flat_map(|e| e.parts.iter())
            .filter(|p| matches!(p, ContentPart::Image { .. }))
            .count();
        // ALL system entries, in window order — not just the first. The chat
        // path pushes the canonical prompt, then `<agent-context-memory>`, then
        // (every Nth turn) the memory nudge; a `.find()` here silently dropped
        // everything after the first, so context memory and the nudge never
        // reached the model on streaming turns.
        let system_entries: Vec<&ContextEntry> = active
            .iter()
            .copied()
            .filter(|e| e.role == ContextRole::System && !e.text().trim().is_empty())
            .collect();

        let (anthropic_tools, intent_by_tool, _) = Self::build_anthropic_tools(tools, None); // ponytail: no options on the stream path → no deferral

        // Streaming path applies the same prompt-caching breakpoints used by
        // the non-streaming path: system block, last tool, and conversation
        // prefix (next-to-last message). The streaming trait method does not
        // receive `InferenceOptions`, so caching is always on for Anthropic
        // streams — matches the kernel's policy of always-on Anthropic caching.
        //
        // One block PER ENTRY, with the breakpoint on the stable prefix — never
        // a single merged block. Merging would put the volatile entries
        // (`<agent-context-memory>`, which changes on any memory write, and the
        // every-Nth-turn nudge) *inside* the cached block, so the cached prefix
        // text would differ between turns and every turn would miss the cache
        // and pay a fresh write at 1.25x. Mirrors `infer_with_options`.
        let system_value = if !system_entries.is_empty() {
            let mut blocks: Vec<Value> = Vec::new();
            let mut breakpoint_set = false;
            for entry in &system_entries {
                let mut block = json!({
                    "type": "text",
                    "text": entry.text(),
                });
                if !breakpoint_set && entry.category == ContextCategory::Tools {
                    block["cache_control"] = json!({ "type": "ephemeral" });
                    breakpoint_set = true;
                }
                blocks.push(block);
            }
            if !breakpoint_set {
                blocks[0]["cache_control"] = json!({ "type": "ephemeral" });
            }
            Value::Array(blocks)
        } else {
            Value::Null
        };
        if messages.len() >= 2 {
            let idx = messages.len() - 2;
            attach_cache_control_to_last_block(&mut messages[idx], PromptCacheTtl::FiveMinutes);
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system_value,
            "messages": messages,
            "stream": true,
        });
        if !anthropic_tools.is_empty() {
            let mut tools_array = anthropic_tools;
            if let Some(last) = tools_array.last_mut() {
                last["cache_control"] = json!({ "type": "ephemeral" });
            }
            body["tools"] = Value::Array(tools_array);
            body["tool_choice"] = json!({"type": "auto"});
        }

        tracing::debug!(
            target: "agentos::llm::input",
            model = %self.model,
            image_count,
            "LLM input body (stream): {}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string())
        );

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
            "anthropic",
            &self.retry_policy,
            &self.circuit_breaker,
            Some(&self.concurrency),
            || {
                self.client
                    .post(&url)
                    .header("x-api-key", self.api_key.expose_secret())
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .json(&body)
            },
        )
        .await;
        let (res, _permit) = match res {
            Ok(r) => r,
            Err(e) => {
                self.note_deferral_rejection(&e, false);
                let _ = tx.send(InferenceEvent::Error(e.to_string())).await;
                return Err(e);
            }
        };

        // Streaming state.
        let mut full_text = String::new();
        let mut tool_calls: Vec<InferenceToolCall> = Vec::new();
        let mut usage = TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        let mut cached_tokens: u64 = 0;
        let mut cache_write_tokens: u64 = 0;
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
                            cache_write_tokens = u
                                .get("cache_creation_input_tokens")
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

                            let payload = tool_helpers::validate_payload_object(
                                &tool_name,
                                "anthropic",
                                Some(payload),
                            );
                            if !tool_helpers::check_payload_size(&tool_name, &payload) {
                                tool_block_index += 1;
                                current_tool_args_buffer.clear();
                                current_block_type = None;
                                continue;
                            }

                            let tc = InferenceToolCall {
                                id: current_tool_id.take(),
                                tool_name: tool_name.clone(),
                                intent_type,
                                payload,
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
        // Weight cache-write/read tokens into the cost basis (see non-streaming
        // path) so streamed inferences bill true spend for budget enforcement (H2).
        let cost = calculate_inference_cost(
            &weighted_usage_for_cost(
                usage.prompt_tokens,
                usage.completion_tokens,
                cache_write_tokens,
                cached_tokens,
            ),
            &self.pricing,
        );

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
            parts: vec![ContentPart::Text {
                text: "System rules here.".to_string(),
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
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![ContentPart::Text {
                text: "Hello".to_string(),
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

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx, true);

        // System prompt is separated in Anthropic
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().expect("array content");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello");
    }

    #[test]
    fn anthropic_format_messages_includes_png_image_block() {
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
            .unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![
                ContentPart::Text {
                    text: "Describe it".into(),
                },
                ContentPart::Image {
                    mime: "image/png".into(),
                    source: ImageSource::Base64 { data },
                },
            ],
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
        let messages = adapter.format_messages(&ctx, true);
        let content = messages[0]["content"].as_array().expect("array");
        assert!(content.iter().any(|b| b["type"] == "image"));
        let img = content.iter().find(|b| b["type"] == "image").unwrap();
        assert_eq!(img["source"]["type"], "base64");
        assert_eq!(img["source"]["media_type"], "image/png");
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
            usage_hints: None,
            tags: vec![],
        };

        let (tools, intent_map, _) =
            AnthropicCore::build_anthropic_tools(&[manifest.clone(), manifest.clone()], None);
        {
            // Provider-native deferral: only the tail at index ≥ n is flagged.
            let mut other = manifest.clone();
            other.manifest.name = "shell-exec".to_string();
            let (deferred, _, _) =
                AnthropicCore::build_anthropic_tools(&[manifest, other], Some(1));
            assert!(deferred[0].get("defer_loading").is_none());
            assert_eq!(deferred[1]["defer_loading"], true);
        }
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "file-reader");
        assert_eq!(intent_map.get("file-reader"), Some(&"read".to_string()));

        // Golden-body assertion: Anthropic Messages API requires `input_schema`
        // (NOT `payload_schema`). Using the wrong key silently produces schemaless
        // tools or 400 errors. This is the regression test for that bug.
        assert!(
            tools[0].get("input_schema").is_some(),
            "Anthropic tool def must use `input_schema` key; got: {}",
            tools[0]
        );
        assert!(
            tools[0].get("payload_schema").is_none(),
            "Anthropic tool def must NOT carry `payload_schema` — that's the AgentOS-internal field name"
        );
        assert_eq!(tools[0]["input_schema"]["type"], "object");
        assert!(tools[0]["input_schema"]["properties"]["path"].is_object());
    }

    #[test]
    fn test_format_messages_native_tool_result() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![ContentPart::Text {
                text: "Read the file".to_string(),
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
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: "file contents here".to_string(),
            }],
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
        let messages = adapter.format_messages(&ctx, true);

        assert_eq!(messages.len(), 2);
        // First message is the user message
        assert_eq!(messages[0]["role"], "user");
        let first_content = messages[0]["content"].as_array().expect("array content");
        assert_eq!(first_content[0]["type"], "text");
        assert_eq!(first_content[0]["text"], "Read the file");
        // Second is the tool result batch (user role with content blocks)
        assert_eq!(messages[1]["role"], "user");
        let content = messages[1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_abc123");
        assert_eq!(content[0]["content"], "file contents here");
    }

    #[test]
    fn test_format_messages_renders_tool_references_for_deferred_tools() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: "{\"matches\":[{\"name\":\"web-fetch\"}]}".to_string(),
            }],
            metadata: Some(ContextMetadata {
                tool_name: Some("search-tools".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("toolu_search".to_string()),
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
        ctx.tool_references
            .insert("toolu_search".to_string(), vec!["web-fetch".to_string()]);

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx, true);
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        let inner = content[0]["content"]
            .as_array()
            .expect("text + tool_reference blocks");
        assert_eq!(inner[0]["type"], "text");
        assert_eq!(inner[1]["type"], "tool_reference");
        assert_eq!(inner[1]["tool_name"], "web-fetch");
    }

    #[test]
    fn test_format_messages_omits_tool_references_when_deferral_inactive() {
        // Same context as above, but the request is not sending the full
        // catalogue (400 fallback / final synthesis / stream path): the
        // reference would name a tool absent from `tools` → must be plain text.
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: "{\"matches\":[{\"name\":\"web-fetch\"}]}".to_string(),
            }],
            metadata: Some(ContextMetadata {
                tool_name: Some("search-tools".to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: Some("toolu_search".to_string()),
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
        ctx.tool_references
            .insert("toolu_search".to_string(), vec!["web-fetch".to_string()]);
        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx, false);
        let content = messages[0]["content"].as_array().unwrap();
        assert!(content[0]["content"].is_string(), "{:?}", content[0]);
    }

    #[test]
    fn test_format_messages_legacy_tool_result_without_metadata() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: "status: ok".to_string(),
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

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx, true);

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
            parts: vec![ContentPart::Text {
                text: "result A".to_string(),
            }],
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
            parts: vec![ContentPart::Text {
                text: "result B".to_string(),
            }],
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
        let messages = adapter.format_messages(&ctx, true);

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
            parts: vec![ContentPart::Text {
                text: "native result".to_string(),
            }],
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
            parts: vec![ContentPart::Text {
                text: "legacy result".to_string(),
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

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx, true);

        // Both should be in a single user message (no consecutive user messages)
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "Tool Result:\nlegacy result");
    }

    #[test]
    fn test_attach_cache_control_to_array_content() {
        let mut msg = json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "text", "text": "world" }
            ]
        });
        attach_cache_control_to_last_block(&mut msg, PromptCacheTtl::FiveMinutes);
        let blocks = msg["content"].as_array().unwrap();
        assert!(blocks[0].get("cache_control").is_none());
        assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_attach_cache_control_to_string_content_rewrites_to_array() {
        let mut msg = json!({
            "role": "user",
            "content": "hello"
        });
        attach_cache_control_to_last_block(&mut msg, PromptCacheTtl::FiveMinutes);
        let blocks = msg["content"].as_array().expect("rewritten to array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "hello");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_cache_control_value_emits_ttl_only_for_one_hour() {
        // 5m must stay byte-identical to the legacy marker — no `ttl` key.
        assert_eq!(
            cache_control_value(PromptCacheTtl::FiveMinutes),
            json!({ "type": "ephemeral" })
        );
        assert_eq!(
            cache_control_value(PromptCacheTtl::OneHour),
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
    }

    #[test]
    fn test_attach_cache_control_one_hour_carries_ttl() {
        let mut msg = json!({ "role": "user", "content": "hello" });
        attach_cache_control_to_last_block(&mut msg, PromptCacheTtl::OneHour);
        let blocks = msg["content"].as_array().expect("rewritten to array");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(blocks[0]["cache_control"]["ttl"], "1h");
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

/// The tool definition that carries cache breakpoint #2: the last tool of the
/// stable prefix (`prefix_len`), or the last tool when no prefix is declared
/// or it is out of range. Tools appended after the prefix (armed on demand)
/// then leave the cached block untouched.
fn cache_breakpoint_tool(
    tools: &mut [serde_json::Value],
    prefix_len: Option<usize>,
) -> Option<&mut serde_json::Value> {
    let idx = match prefix_len {
        Some(n) if n > 0 && n <= tools.len() => n - 1,
        _ => tools.len().checked_sub(1)?,
    };
    tools.get_mut(idx)
}

/// Models that accept `defer_loading` / `tool_reference` (Anthropic tool
/// search): Sonnet/Haiku/Opus 4.5 and later, Opus/Sonnet 4.6–5, Fable/Mythos 5.
/// Opus 4.1 and earlier reject the field with a 400.
fn model_supports_deferred_tools(model: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "claude-sonnet-4-5",
        "claude-haiku-4-5",
        "claude-opus-4-5",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-haiku-5",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    let m = model.trim().to_ascii_lowercase();
    PREFIXES.iter().any(|p| m.starts_with(p))
}

/// Map an input-position boundary (`n` = "first `n` input tools") to the
/// output position after `build_anthropic_tools` dropped blank/duplicate
/// names, so the breakpoint never slides into the deferred tail.
fn kept_index(kept: &[bool], n: Option<usize>) -> Option<usize> {
    n.map(|n| kept[..n.min(kept.len())].iter().filter(|k| **k).count())
}

/// Where the tools cache breakpoint goes: the end of the stable prefix, which
/// can never be inside the deferred tail (`defer_loading` + `cache_control` on
/// one tool is a 400).
fn deferral_cache_prefix(prefix_len: Option<usize>, deferred_from: Option<usize>) -> Option<usize> {
    match (prefix_len, deferred_from) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

impl AnthropicCore {
    /// A 400 mentioning `defer_loading`/`tool_reference` means this endpoint
    /// (proxy, older model) does not support tool search: flip to kernel
    /// emulation for this adapter instance rather than failing every turn.
    ///
    /// `requested`: this request carried `defer_loading`. Any 400 then counts:
    /// a proxy that does not understand the field may answer with a generic
    /// message, and repeating the request every iteration helps nobody.
    fn note_deferral_rejection(&self, e: &AgentOSError, requested: bool) {
        let msg = e.to_string();
        if msg.contains("API error 400")
            && (requested || msg.contains("defer_loading") || msg.contains("tool_reference"))
            && !self.deferral_rejected.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                model = %self.model,
                "Anthropic rejected deferred tool loading — sending every tool inline for this adapter instance"
            );
        }
    }
}

#[cfg(test)]
mod deferral_tests {
    use super::*;

    #[test]
    fn model_gate_matches_tool_search_capable_models_only() {
        for m in [
            "claude-opus-5",
            "claude-fable-5-1",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
        ] {
            assert!(model_supports_deferred_tools(m), "{m}");
        }
        for m in [
            "claude-opus-4-1",
            "claude-3-5-sonnet-20241022",
            "claude-sonnet-4-20250514",
            "",
        ] {
            assert!(!model_supports_deferred_tools(m), "{m}");
        }
    }

    #[test]
    fn cache_prefix_never_reaches_into_deferred_tail() {
        assert_eq!(deferral_cache_prefix(None, None), None);
        assert_eq!(deferral_cache_prefix(Some(7), None), Some(7));
        assert_eq!(deferral_cache_prefix(Some(7), Some(5)), Some(5));
        assert_eq!(deferral_cache_prefix(None, Some(5)), Some(5));
    }
}

#[cfg(test)]
mod cache_breakpoint_tests {
    use super::cache_breakpoint_tool;
    use serde_json::json;

    #[test]
    fn prefix_len_pins_breakpoint_so_appended_tools_do_not_move_it() {
        let mut tools = vec![
            json!({"name":"a"}),
            json!({"name":"b"}),
            json!({"name":"c"}),
        ];
        let bp = cache_breakpoint_tool(&mut tools, Some(2)).unwrap();
        assert_eq!(bp["name"], "b");
        // Arm one more tool after the prefix — breakpoint still on "b".
        tools.push(json!({"name":"d"}));
        assert_eq!(
            cache_breakpoint_tool(&mut tools, Some(2)).unwrap()["name"],
            "b"
        );
        // Legacy / out-of-range → last tool.
        assert_eq!(
            cache_breakpoint_tool(&mut tools, None).unwrap()["name"],
            "d"
        );
        assert_eq!(
            cache_breakpoint_tool(&mut tools, Some(9)).unwrap()["name"],
            "d"
        );
        assert_eq!(
            cache_breakpoint_tool(&mut tools, Some(0)).unwrap()["name"],
            "d"
        );
        assert!(cache_breakpoint_tool(&mut [], Some(1)).is_none());
    }
}

#[cfg(test)]
mod kept_index_tests {
    use super::{deferral_cache_prefix, kept_index};

    #[test]
    fn kept_index_maps_input_boundary_past_dropped_entries() {
        // input: [a, "", b, a(dup), c] → kept [t, f, t, f, t] → output [a, b, c]
        let kept = [true, false, true, false, true];
        assert_eq!(kept_index(&kept, Some(3)), Some(2));
        assert_eq!(kept_index(&kept, Some(4)), Some(2));
        assert_eq!(kept_index(&kept, Some(5)), Some(3));
        assert_eq!(kept_index(&kept, Some(99)), Some(3));
        assert_eq!(kept_index(&kept, None), None);
        assert_eq!(
            deferral_cache_prefix(kept_index(&kept, Some(3)), kept_index(&kept, Some(3))),
            Some(2)
        );
    }
}
