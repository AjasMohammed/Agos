use crate::traits::LLMCore;
use crate::types::{InferenceResult, ModelCapabilities, TokenUsage};
use agentos_types::*;
use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use std::time::Instant;

/// Anthropic API adapter for Claude models.
pub struct AnthropicCore {
    client: Client,
    api_key: SecretString,
    model: String,
    capabilities: ModelCapabilities,
}

impl AnthropicCore {
    pub fn new(api_key: SecretString, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            capabilities: ModelCapabilities {
                context_window_tokens: 200_000,
                supports_images: true,
                supports_tool_calling: true,
                supports_json_mode: false, // Anthropic handles JSON via instructions, not strict mode usually
            },
        }
    }

    fn format_messages(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();

        for entry in context.as_entries() {
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
}

#[async_trait]
impl LLMCore for AnthropicCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let start_time = Instant::now();
        let url = "https://api.anthropic.com/v1/messages";

        let messages = self.format_messages(context);
        let system_prompt = context
            .as_entries()
            .iter()
            .find(|e| e.role == ContextRole::System)
            .map(|e| e.content.as_str())
            .unwrap_or("");

        let body = json!({
            "model": self.model,
            "max_tokens": 4096, // required by Anthropic
            "system": system_prompt,
            "messages": messages,
        });

        let req = self
            .client
            .post(url)
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

        // Extract content blocks
        let mut text = String::new();
        if let Some(content_array) = json_resp["content"].as_array() {
            for block in content_array {
                if let Some(t) = block["text"].as_str() {
                    text.push_str(t);
                }
            }
        }

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
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        use crate::types::HealthStatus;
        let start = std::time::Instant::now();
        let url = "https://api.anthropic.com/v1/messages";
        let body = json!({
            "model": self.model,
            "max_tokens": 1,
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });

        match self
            .client
            .post(url)
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
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
        });

        let adapter = AnthropicCore::new(SecretString::new("fake".into()), "claude".into());
        let messages = adapter.format_messages(&ctx);

        // System prompt is separated in Anthropic
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
    }
}
