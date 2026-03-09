use crate::traits::LLMCore;
use crate::types::{InferenceEvent, InferenceResult, ModelCapabilities, TokenUsage};
use agentos_types::*;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub struct OllamaCore {
    client: Client,
    host: String,
    model: String,
    capabilities: ModelCapabilities,
}

impl OllamaCore {
    pub fn new(host: &str, model: &str) -> Self {
        Self {
            client: Client::new(),
            host: host.to_string(),
            model: model.to_string(),
            capabilities: ModelCapabilities {
                context_window_tokens: 8192,
                supports_images: false,
                supports_tool_calling: false,
                supports_json_mode: true,
            },
        }
    }
}

// --- Ollama REST API types (private) ---

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaChatMessage>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OllamaChatResponse {
    model: String,
    message: OllamaChatMessage,
    done: bool,
    total_duration: Option<u64>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
}

#[async_trait]
impl LLMCore for OllamaCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let start = std::time::Instant::now();

        // Convert ContextWindow to Ollama chat messages format
        let messages: Vec<OllamaChatMessage> = context
            .as_entries()
            .iter()
            .map(|entry| OllamaChatMessage {
                role: match entry.role {
                    ContextRole::System => "system".to_string(),
                    ContextRole::User => "user".to_string(),
                    ContextRole::Assistant => "assistant".to_string(),
                    ContextRole::ToolResult => "user".to_string(), // tool results sent as user messages
                },
                content: entry.content.clone(),
            })
            .collect();

        let request = OllamaChatRequest {
            model: self.model.clone(),
            messages,
            stream: false,
        };

        let response = self
            .client
            .post(format!("{}/api/chat", self.host))
            .json(&request)
            .send()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Request failed: {}", e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("API Error {}: {}", status, body),
            });
        }

        let ollama_response: OllamaChatResponse =
            response.json().await.map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Failed to parse JSON response: {}", e),
            })?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(InferenceResult {
            text: ollama_response.message.content,
            tokens_used: TokenUsage {
                prompt_tokens: ollama_response.prompt_eval_count.unwrap_or(0),
                completion_tokens: ollama_response.eval_count.unwrap_or(0),
                total_tokens: ollama_response.prompt_eval_count.unwrap_or(0)
                    + ollama_response.eval_count.unwrap_or(0),
            },
            model: self.model.clone(),
            duration_ms,
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        use crate::types::HealthStatus;
        let start = std::time::Instant::now();
        match self
            .client
            .get(format!("{}/api/tags", self.host))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let latency = start.elapsed();
                if latency > std::time::Duration::from_secs(2) {
                    HealthStatus::Degraded {
                        reason: format!("High latency: {}ms", latency.as_millis()),
                    }
                } else {
                    HealthStatus::Healthy
                }
            }
            Ok(resp) => HealthStatus::Unhealthy {
                reason: format!("HTTP {}", resp.status()),
            },
            Err(e) => HealthStatus::Unhealthy {
                reason: format!("Connection failed: {e}"),
            },
        }
    }

    async fn infer_stream(
        &self,
        context: &ContextWindow,
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        let start = std::time::Instant::now();

        let messages: Vec<OllamaChatMessage> = context
            .as_entries()
            .iter()
            .map(|entry| OllamaChatMessage {
                role: match entry.role {
                    ContextRole::System => "system".to_string(),
                    ContextRole::User => "user".to_string(),
                    ContextRole::Assistant => "assistant".to_string(),
                    ContextRole::ToolResult => "user".to_string(),
                },
                content: entry.content.clone(),
            })
            .collect();

        let request = OllamaChatRequest {
            model: self.model.clone(),
            messages,
            stream: true,
        };

        let response = self
            .client
            .post(format!("{}/api/chat", self.host))
            .json(&request)
            .send()
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Request failed: {}", e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let err = AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("API Error {}: {}", status, body),
            };
            let _ = tx.send(InferenceEvent::Error(err.to_string())).await;
            return Err(err);
        }

        let mut full_text = String::new();
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;

        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::LLMError {
                provider: "ollama".to_string(),
                reason: format!("Stream read error: {}", e),
            })?;

            // Ollama sends newline-delimited JSON
            for line in chunk.split(|&b| b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                if let Ok(resp) = serde_json::from_slice::<OllamaChatResponse>(line) {
                    if !resp.message.content.is_empty() {
                        full_text.push_str(&resp.message.content);
                        let _ = tx
                            .send(InferenceEvent::Token(resp.message.content))
                            .await;
                    }
                    if resp.done {
                        prompt_tokens = resp.prompt_eval_count.unwrap_or(0);
                        completion_tokens = resp.eval_count.unwrap_or(0);
                    }
                }
            }
        }

        let result = InferenceResult {
            text: full_text,
            tokens_used: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
            model: self.model.clone(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
        let _ = tx.send(InferenceEvent::Done(result)).await;
        Ok(())
    }

    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_to_messages_conversion() {
        let mut ctx = ContextWindow::new(100);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "You are a helpful assistant.".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello!".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });

        let entries = ctx.as_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, ContextRole::System);
        assert_eq!(entries[1].role, ContextRole::User);
    }

    #[tokio::test]
    #[ignore] // only run when Ollama is available
    async fn test_ollama_health_check() {
        let ollama = OllamaCore::new("http://localhost:11434", "llama3.2");
        let status = ollama.health_check().await;
        assert!(status.is_healthy(), "Ollama should be running on localhost:11434");
    }

    #[tokio::test]
    #[ignore]
    async fn test_ollama_infer() {
        let ollama = OllamaCore::new("http://localhost:11434", "llama3.2");

        let mut ctx = ContextWindow::new(100);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Say 'hello' and nothing else.".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });

        let result = ollama.infer(&ctx).await.unwrap();
        assert!(!result.text.is_empty());
        assert!(result.tokens_used.total_tokens > 0);
    }
}
