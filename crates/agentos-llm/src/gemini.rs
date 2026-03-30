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

/// Gemini API adapter for Google models.
pub struct GeminiCore {
    client: Client,
    api_key: SecretString,
    model: String,
    capabilities: ModelCapabilities,
    pricing: ModelPricing,
    retry_policy: crate::retry::RetryPolicy,
    circuit_breaker: crate::retry::CircuitBreaker,
}

impl GeminiCore {
    pub fn new(api_key: SecretString, model: String) -> Self {
        let table = default_pricing_table();
        let pricing = table
            .iter()
            .find(|p| p.provider == "gemini" && p.model == model)
            .or_else(|| {
                table
                    .iter()
                    .find(|p| p.provider == "gemini" && p.model == "*")
            })
            .cloned()
            .unwrap_or(ModelPricing {
                provider: "gemini".to_string(),
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
            capabilities: ModelCapabilities {
                context_window_tokens: 1_000_000,
                supports_images: true,
                supports_tool_calling: true,
                supports_json_mode: true,
                max_output_tokens: 0,
                supports_streaming: true,
                supports_parallel_tools: true,
                supports_prompt_caching: false,
                supports_thinking: true,
                supports_structured_output: true,
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

    fn format_contents(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut contents = Vec::new();

        for entry in context.active_entries() {
            match entry.role {
                ContextRole::System => continue, // System instructions are passed separately
                ContextRole::ToolResult => {
                    let tool_name = entry.metadata.as_ref().and_then(|m| m.tool_name.as_deref());

                    if let Some(name) = tool_name {
                        // Native Gemini functionResponse format.
                        // Parse content as JSON for structured response, fallback to wrapper.
                        let response_val = serde_json::from_str::<Value>(&entry.content)
                            .unwrap_or_else(|_| json!({"result": entry.content}));
                        contents.push(json!({
                            "role": "user",
                            "parts": [{
                                "functionResponse": {
                                    "name": name,
                                    "response": response_val,
                                }
                            }]
                        }));
                    } else {
                        // Legacy fallback.
                        contents.push(json!({
                            "role": "user",
                            "parts": [{"text": format!("Tool Result:\n{}", entry.content)}]
                        }));
                    }
                }
                ContextRole::User => {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{"text": entry.content.clone()}]
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
                        if !entry.content.is_empty() {
                            parts.push(json!({"text": entry.content.clone()}));
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
                            "parts": [{"text": entry.content.clone()}]
                        }));
                    }
                }
            }
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
                "parameters": tool_helpers::normalize_tool_input_schema(manifest.input_schema.as_ref()),
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
            if let Some(t) = part.get("text").and_then(Value::as_str) {
                text.push_str(t);
            }
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
        if let Some(sys) = active
            .iter()
            .find(|e| e.role == ContextRole::System)
            .map(|e| e.content.as_str())
        {
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
        if !gen_config.is_empty() {
            body["generationConfig"] = Value::Object(gen_config);
        }

        let res = crate::retry::send_with_retry(
            "gemini",
            &self.retry_policy,
            &self.circuit_breaker,
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

        let contents = self.format_contents(context);
        let (function_declarations, intent_by_tool) = Self::build_gemini_tools(tools);

        let mut body = json!({ "contents": contents });

        let active = context.active_entries();
        if let Some(sys) = active
            .iter()
            .find(|e| e.role == ContextRole::System)
            .map(|e| e.content.as_str())
        {
            body["systemInstruction"] = json!({ "parts": [{"text": sys}] });
        }
        if !function_declarations.is_empty() {
            body["tools"] = json!([{ "functionDeclarations": function_declarations }]);
        }

        let res = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", self.api_key.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "gemini".to_string(),
                reason: format!("Reqwest failed: {}", e),
            })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            let err_msg = format!("Gemini API error {}: {}", status, text);
            let _ = tx.send(InferenceEvent::Error(err_msg.clone())).await;
            return Err(AgentOSError::LLMError {
                provider: "gemini".to_string(),
                reason: err_msg,
            });
        }

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

        let mut stream = res.bytes_stream();
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "gemini".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&chunk_str);

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
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            full_text.push_str(t);
                            let _ = tx.send(InferenceEvent::Token(t.to_string())).await;
                        }
                    }
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
            content: "System".to_string(),
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
            content: "User".to_string(),
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
            content: "Assistant".to_string(),
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
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: r#"{"status": "ok"}"#.to_string(),
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

        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        let fr = &parts[0]["functionResponse"];
        assert_eq!(fr["name"], "file-reader");
        assert_eq!(fr["response"]["status"], "ok");
    }

    #[test]
    fn test_format_contents_native_tool_result_plain_text() {
        // Non-JSON content wraps in {"result": "..."}
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "plain text result".to_string(),
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

        let fr = &contents[0]["parts"][0]["functionResponse"];
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
}
