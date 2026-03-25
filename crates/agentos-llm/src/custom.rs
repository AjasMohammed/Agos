use crate::traits::LLMCore;
use crate::types::{
    calculate_inference_cost, default_pricing_table, InferenceResult, ModelCapabilities,
    ModelPricing, StopReason, TokenUsage,
};
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
    pricing: ModelPricing,
    retry_policy: crate::retry::RetryPolicy,
    circuit_breaker: crate::retry::CircuitBreaker,
}

impl CustomCore {
    /// Create a new Custom adapter.
    pub fn new(api_key: Option<SecretString>, model: String, base_url: String) -> Self {
        // No entry in the default pricing table for custom providers — zero-cost fallback.
        // Allow overriding from the default table if a "custom" entry is added in the future.
        // Prefers exact model match over wildcard to avoid incorrect pricing.
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
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("HTTP client TLS initialization failed"),
            api_key,
            model,
            base_url,
            capabilities: ModelCapabilities {
                context_window_tokens: 32768,
                supports_images: false,
                supports_tool_calling: false,
                supports_json_mode: false,
                max_output_tokens: 0,
                supports_streaming: false,
                supports_parallel_tools: false,
                supports_prompt_caching: false,
                supports_thinking: false,
                supports_structured_output: false,
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
        // Pre-flight: custom endpoints are often paid APIs with real context limits.
        let estimated = self.estimate_tokens(context, &[]);
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
        let url = format!("{}/chat/completions", self.base_url);
        let messages = self.format_messages(context);

        let body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false
        });

        let res = crate::retry::send_with_retry(
            "custom",
            &self.retry_policy,
            &self.circuit_breaker,
            || {
                let mut req = self
                    .client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .json(&body);
                if let Some(key) = &self.api_key {
                    req = req.header("Authorization", format!("Bearer {}", key.expose_secret()));
                }
                req
            },
        )
        .await?;

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

        // Extract stop reason from OpenAI-compatible response
        let finish_reason = json_resp["choices"][0]["finish_reason"]
            .as_str()
            .unwrap_or("stop");
        let stop_reason = match finish_reason {
            "stop" => StopReason::EndTurn,
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            "content_filter" => StopReason::ContentFilter,
            other => StopReason::Other(other.to_string()),
        };

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
            tool_calls: Vec::new(),
            uncertainty: None,
            stop_reason,
            cost: Some(cost),
            cached_tokens: 0,
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
