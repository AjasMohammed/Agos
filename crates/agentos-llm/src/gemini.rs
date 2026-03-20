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

/// Gemini API adapter for Google models.
pub struct GeminiCore {
    client: Client,
    api_key: SecretString,
    model: String,
    capabilities: ModelCapabilities,
}

impl GeminiCore {
    pub fn new(api_key: SecretString, model: String) -> Self {
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
            },
        }
    }

    fn format_contents(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut contents = Vec::new();

        // Gemini uses 'user' and 'model' roles.
        for entry in context.active_entries() {
            let role = match entry.role {
                ContextRole::User => "user",
                ContextRole::Assistant => "model",
                ContextRole::ToolResult => "user",
                ContextRole::System => continue, // System instructions are passed separately
            };

            let parts = match entry.role {
                ContextRole::ToolResult => format!("Tool Result:\n{}", entry.content),
                _ => entry.content.clone(),
            };

            contents.push(json!({
                "role": role,
                "parts": [{"text": parts}]
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
        let start_time = Instant::now();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            self.model,
        );

        let contents = self.format_contents(context);
        let (function_declarations, intent_by_tool) = Self::build_gemini_tools(tools);

        let mut body = json!({
            "contents": contents,
        });

        let active = context.active_entries();
        if let Some(sys) = active
            .iter()
            .find(|e| e.role == ContextRole::System)
            .map(|e| e.content.as_str())
        {
            body["systemInstruction"] = json!({
                "parts": [{"text": sys}]
            });
        }

        if !function_declarations.is_empty() {
            body["tools"] = json!([{
                "functionDeclarations": function_declarations
            }]);
        }

        let req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", self.api_key.expose_secret())
            .json(&body);

        let res = req.send().await.map_err(|e| AgentOSError::LLMError {
            provider: "gemini".to_string(),
            reason: format!("Reqwest failed: {}", e),
        })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "gemini".to_string(),
                reason: format!("Gemini API error {}: {}", status, text),
            });
        }

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
        let text = tool_helpers::append_legacy_blocks(&text, &tool_calls);

        let prompt_tokens = json_resp["usageMetadata"]["promptTokenCount"]
            .as_u64()
            .unwrap_or(0);
        let completion_tokens = json_resp["usageMetadata"]["candidatesTokenCount"]
            .as_u64()
            .unwrap_or(0);
        let total_tokens = json_resp["usageMetadata"]["totalTokenCount"]
            .as_u64()
            .unwrap_or(prompt_tokens + completion_tokens);

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
}
