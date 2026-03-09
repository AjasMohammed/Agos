use crate::traits::LLMCore;
use crate::types::{InferenceResult, ModelCapabilities, TokenUsage};
use agentos_types::*;
use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use std::time::Instant;

/// OpenAI API adapter for models like gpt-4o, gpt-3.5-turbo, etc.
pub struct OpenAICore {
    client: Client,
    api_key: SecretString,
    model: String,
    base_url: String,
    capabilities: ModelCapabilities,
}

impl OpenAICore {
    /// Create a new OpenAI adapter using the default base URL.
    pub fn new(api_key: SecretString, model: String) -> Self {
        Self::with_base_url(api_key, model, "https://api.openai.com/v1".to_string())
    }

    /// Create a new OpenAI adapter with a custom base URL.
    pub fn with_base_url(api_key: SecretString, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url,
            capabilities: ModelCapabilities {
                context_window_tokens: 128_000,
                supports_images: true, // Typical for modern OpenAI models
                supports_tool_calling: true,
                supports_json_mode: true,
            },
        }
    }

    /// Convert our internal `ContextWindow` to OpenAI's messages array format.
    fn format_messages(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();

        for entry in context.as_entries() {
            let role = match entry.role {
                ContextRole::User => "user",
                ContextRole::Assistant => "assistant",
                ContextRole::ToolResult => "user", // OpenAI doesn't have a distinct role for tool execution outputs without their specific tool call machinery, so we pass it as user/system equivalent.
                ContextRole::System => "system",
            };

            let content = match entry.role {
                ContextRole::ToolResult => {
                    // Prepend label
                    format!("Tool Result:\n{}", entry.content)
                }
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
impl LLMCore for OpenAICore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let start_time = Instant::now();
        let url = format!("{}/chat/completions", self.base_url);
        let messages = self.format_messages(context);

        let body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false
        });

        let req = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .header("Content-Type", "application/json")
            .json(&body);

        let res = req.send().await.map_err(|e| AgentOSError::LLMError {
            provider: "openai".to_string(),
            reason: format!("Reqwest failed: {}", e),
        })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!("OpenAI API error {}: {}", status, text),
            });
        }

        let json_resp: serde_json::Value =
            res.json().await.map_err(|e| AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!("Failed to parse JSON response: {}", e),
            })?;

        let text = json_resp["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let prompt_tokens = json_resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        let completion_tokens = json_resp["usage"]["completion_tokens"]
            .as_u64()
            .unwrap_or(0);
        let total_tokens = json_resp["usage"]["total_tokens"].as_u64().unwrap_or(0);

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
        let url = format!("{}/models", self.base_url);
        match self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
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
        "openai"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_messages() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "You are a helpful assistant.".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
        });
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "status: ok".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
        });

        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4".into());
        let messages = adapter.format_messages(&ctx);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"], "Tool Result:\nstatus: ok");
    }
}
