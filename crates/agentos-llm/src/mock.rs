use crate::traits::LLMCore;
use crate::types::{InferenceResult, ModelCapabilities, TokenUsage};
use agentos_types::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// A mock LLM backend for testing. Returns preconfigured responses in sequence.
pub struct MockLLMCore {
    responses: Mutex<Vec<String>>,
    call_count: AtomicUsize,
    capabilities: ModelCapabilities,
}

impl MockLLMCore {
    /// Create a new mock with the given responses. Each call to `infer()` pops the next response.
    /// If all responses are exhausted, returns "No more mock responses".
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(responses),
            call_count: AtomicUsize::new(0),
            capabilities: ModelCapabilities {
                context_window_tokens: 8192,
                supports_images: false,
                supports_tool_calling: false,
                supports_json_mode: false,
            },
        }
    }

    /// How many times `infer()` has been called.
    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LLMCore for MockLLMCore {
    async fn infer(&self, _context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);

        let text = {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                "No more mock responses".to_string()
            } else {
                responses.remove(0)
            }
        };

        Ok(InferenceResult {
            text,
            tokens_used: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            model: "mock-model".to_string(),
            duration_ms: 1,
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        crate::types::HealthStatus::Healthy
    }

    fn provider_name(&self) -> &str {
        "mock"
    }

    fn model_name(&self) -> &str {
        "mock-model"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_returns_responses_in_order() {
        let mock = MockLLMCore::new(vec![
            "First response".to_string(),
            "Second response".to_string(),
        ]);

        let ctx = ContextWindow::new(10);
        let r1 = mock.infer(&ctx).await.unwrap();
        assert_eq!(r1.text, "First response");
        assert_eq!(mock.call_count(), 1);

        let r2 = mock.infer(&ctx).await.unwrap();
        assert_eq!(r2.text, "Second response");
        assert_eq!(mock.call_count(), 2);

        // Exhausted
        let r3 = mock.infer(&ctx).await.unwrap();
        assert_eq!(r3.text, "No more mock responses");
    }

    #[tokio::test]
    async fn test_mock_health_check() {
        let mock = MockLLMCore::new(vec![]);
        assert!(mock.health_check().await.is_healthy());
    }
}
