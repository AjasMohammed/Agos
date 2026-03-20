use crate::tool_helpers;
use crate::traits::LLMCore;
use crate::types::{InferenceResult, InferenceToolCall, ModelCapabilities, TokenUsage};
use agentos_types::*;
use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Anthropic API adapter for Claude models.
pub struct AnthropicCore {
    client: Client,
    api_key: SecretString,
    model: String,
    base_url: String,
    /// Maximum tokens to generate per response. Configurable via `llm.max_tokens`.
    max_tokens: u32,
    capabilities: ModelCapabilities,
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
                supports_json_mode: false, // Anthropic handles JSON via instructions, not strict mode usually
                max_output_tokens: Self::DEFAULT_MAX_TOKENS as u64,
            },
        }
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
        let mut messages = Vec::new();

        for entry in context.active_entries() {
            let role = match entry.role {
                ContextRole::User => "user",
                ContextRole::Assistant => "assistant",
                ContextRole::ToolResult => "user",
                ContextRole::System => continue, // Anthropic wants system prompt top-level
            };

            let content = match entry.role {
                ContextRole::ToolResult => format!("Tool Result:\n{}", entry.content),
                _ => entry.content.clone(),
            };

            messages.push(json!({
                "role": role,
                "content": content,
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
        });

        if !anthropic_tools.is_empty() {
            body["tools"] = Value::Array(anthropic_tools);
            body["tool_choice"] = json!({"type": "auto"});
        }

        let req = self
            .client
            .post(&url)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body);

        let res = req.send().await.map_err(|e| AgentOSError::LLMError {
            provider: "anthropic".to_string(),
            reason: format!("Reqwest failed: {}", e),
        })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "anthropic".to_string(),
                reason: format!("Anthropic API error {}: {}", status, text),
            });
        }

        let json_resp: serde_json::Value =
            res.json().await.map_err(|e| AgentOSError::LLMError {
                provider: "anthropic".to_string(),
                reason: format!("Failed to parse JSON response: {}", e),
            })?;

        let content_blocks = json_resp["content"].as_array().cloned().unwrap_or_default();
        let (text, tool_calls) = Self::parse_anthropic_tool_calls(&content_blocks, &intent_by_tool);
        let text = tool_helpers::append_legacy_blocks(&text, &tool_calls);

        let prompt_tokens = json_resp["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let completion_tokens = json_resp["usage"]["output_tokens"].as_u64().unwrap_or(0);
        let total_tokens = prompt_tokens + completion_tokens;

        Ok(InferenceResult {
            text,
            tokens_used: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            },
            model: self.model.clone(),
            duration_ms: start_time.elapsed().as_millis() as u64,
            tool_calls,
            uncertainty: None,
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
            },
            executor: ToolExecutor::default(),
        };

        let (tools, intent_map) =
            AnthropicCore::build_anthropic_tools(&[manifest.clone(), manifest]);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "file-reader");
        assert_eq!(intent_map.get("file-reader"), Some(&"read".to_string()));
    }
}
