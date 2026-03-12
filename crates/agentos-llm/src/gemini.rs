use crate::traits::LLMCore;
use crate::types::{InferenceResult, ModelCapabilities, TokenUsage};
use agentos_types::*;
use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
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
            client: Client::new(),
            api_key,
            model,
            capabilities: ModelCapabilities {
                context_window_tokens: 1_000_000,
                supports_images: true,
                supports_tool_calling: true,
                supports_json_mode: true,
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
}

#[async_trait]
impl LLMCore for GeminiCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let start_time = Instant::now();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model,
            self.api_key.expose_secret()
        );

        let contents = self.format_contents(context);
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

        let req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
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

        let mut text = String::new();
        if let Some(candidates) = json_resp["candidates"].as_array() {
            if let Some(first) = candidates.first() {
                if let Some(parts) = first["content"]["parts"].as_array() {
                    for part in parts {
                        if let Some(t) = part["text"].as_str() {
                            text.push_str(t);
                        }
                    }
                }
            }
        }

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
            "https://generativelanguage.googleapis.com/v1beta/models/{}?key={}",
            self.model,
            self.api_key.expose_secret()
        );
        match self.client.get(&url).send().await {
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
        });

        let adapter = GeminiCore::new(SecretString::new("fake".into()), "gemini".into());
        let contents = adapter.format_contents(&ctx);

        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "User");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["text"], "Assistant");
    }
}
