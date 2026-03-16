use crate::traits::LLMCore;
use crate::types::{InferenceResult, ModelCapabilities, TokenUsage};
use agentos_types::*;
use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use std::time::Instant;

/// Custom OpenAI-compatible API adapter.
pub struct CustomCore {
    client: Client,
    api_key: Option<SecretString>,
    model: String,
    base_url: String,
    capabilities: ModelCapabilities,
}

impl CustomCore {
    /// Create a new Custom adapter.
    pub fn new(api_key: Option<SecretString>, model: String, base_url: String) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            api_key,
            model,
            base_url,
            capabilities: ModelCapabilities {
                context_window_tokens: 32768, // Conservative default
                supports_images: false,
                supports_tool_calling: false,
                supports_json_mode: false,
            },
        }
    }

    /// Convert our internal `ContextWindow` to messages array (OpenAI style)
    fn format_messages(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();

        for entry in context.active_entries() {
            let role = match entry.role {
                ContextRole::User => "user",
                ContextRole::Assistant => "assistant",
                ContextRole::ToolResult => "user",
                ContextRole::System => "system",
            };

            let content = match entry.role {
                ContextRole::ToolResult => {
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
impl LLMCore for CustomCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let start_time = Instant::now();
        let url = format!("{}/chat/completions", self.base_url);
        let messages = self.format_messages(context);

        let body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false
        });

        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body);

        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key.expose_secret()));
        }

        let res = req.send().await.map_err(|e| AgentOSError::LLMError {
            provider: "custom".to_string(),
            reason: format!("Reqwest failed: {}", e),
        })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "custom".to_string(),
                reason: format!("Custom API error {}: {}", status, text),
            });
        }

        let json_resp: serde_json::Value =
            res.json().await.map_err(|e| AgentOSError::LLMError {
                provider: "custom".to_string(),
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
            uncertainty: None,
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        use crate::types::HealthStatus;
        let start = std::time::Instant::now();
        let url = format!("{}/models", self.base_url);
        let mut req = self.client.get(&url);
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key.expose_secret()));
        }

        match req.send().await {
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
}
