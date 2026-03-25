use crate::traits::LLMCore;
use crate::types::{
    HealthStatus, InferenceEvent, InferenceOptions, InferenceResult, ModelCapabilities,
};
use agentos_types::*;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

/// An LLM adapter that wraps multiple providers and fails over to the next on error.
///
/// Providers are tried in the order they were supplied. The first successful
/// response is returned; remaining providers are never called. If all providers
/// fail, the last error is returned.
///
/// `capabilities()` returns the capabilities of the first (primary) provider.
/// `health_check()` returns `Healthy` if any provider is healthy.
pub struct FallbackAdapter {
    providers: Vec<Arc<dyn LLMCore>>,
    capabilities: ModelCapabilities,
}

impl FallbackAdapter {
    /// Create a new `FallbackAdapter`.
    ///
    /// Returns an error if `providers` is empty.
    pub fn new(providers: Vec<Arc<dyn LLMCore>>) -> Result<Self, AgentOSError> {
        if providers.is_empty() {
            return Err(AgentOSError::LLMError {
                provider: "fallback".to_string(),
                reason: "FallbackAdapter requires at least one provider".to_string(),
            });
        }
        let capabilities = providers[0].capabilities().clone();
        Ok(Self {
            providers,
            capabilities,
        })
    }

    /// Check each provider in order and return the first healthy one.
    ///
    /// Note: this method is only used by `health_check()`. Inference routing
    /// does NOT pre-check health — it tries providers in order and falls over
    /// on actual inference errors.
    async fn find_healthy(&self) -> Option<&Arc<dyn LLMCore>> {
        for provider in &self.providers {
            if provider.health_check().await.is_healthy() {
                return Some(provider);
            }
        }
        None
    }
}

#[async_trait]
impl LLMCore for FallbackAdapter {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        self.infer_with_tools(context, &[]).await
    }

    async fn infer_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
    ) -> Result<InferenceResult, AgentOSError> {
        let mut last_error = None;
        for (i, provider) in self.providers.iter().enumerate() {
            match provider.infer_with_tools(context, tools).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    warn!(
                        provider = provider.provider_name(),
                        index = i,
                        error = %e,
                        "Provider failed, trying next"
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| AgentOSError::LLMError {
            provider: "fallback".to_string(),
            reason: "All providers failed".to_string(),
        }))
    }

    async fn infer_with_options(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        options: &InferenceOptions,
    ) -> Result<InferenceResult, AgentOSError> {
        let mut last_error = None;
        for (i, provider) in self.providers.iter().enumerate() {
            match provider.infer_with_options(context, tools, options).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    warn!(
                        provider = provider.provider_name(),
                        index = i,
                        error = %e,
                        "Provider failed (options), trying next"
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| AgentOSError::LLMError {
            provider: "fallback".to_string(),
            reason: "All providers failed".to_string(),
        }))
    }

    async fn infer_stream(
        &self,
        context: &ContextWindow,
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        self.infer_stream_with_tools(context, &[], tx).await
    }

    async fn infer_stream_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        let mut last_error = None;
        for (i, provider) in self.providers.iter().enumerate() {
            // Use an intermediate channel so partial events from a failing
            // provider are discarded before trying the next one. Without this,
            // a provider that emits tokens before erroring mid-stream would
            // contaminate the caller's channel.
            let (inner_tx, mut inner_rx) = mpsc::channel(256);
            match provider
                .infer_stream_with_tools(context, tools, inner_tx)
                .await
            {
                Ok(()) => {
                    // Forward all buffered events from the successful provider.
                    while let Some(event) = inner_rx.recv().await {
                        let _ = tx.send(event).await;
                    }
                    return Ok(());
                }
                Err(e) => {
                    // inner_rx dropped here, discarding any partial events.
                    warn!(
                        provider = provider.provider_name(),
                        index = i,
                        error = %e,
                        "Streaming provider failed, trying next"
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| AgentOSError::LLMError {
            provider: "fallback".to_string(),
            reason: "All providers failed".to_string(),
        }))
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> HealthStatus {
        if self.find_healthy().await.is_some() {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy {
                reason: "All providers unhealthy".to_string(),
            }
        }
    }

    fn provider_name(&self) -> &str {
        "fallback"
    }

    fn model_name(&self) -> &str {
        self.providers[0].model_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockLLMCore;
    use agentos_types::ContextWindow;

    /// A mock provider that always returns an error on inference
    /// and is always unhealthy.
    struct ErrorMock {
        capabilities: ModelCapabilities,
    }

    impl ErrorMock {
        fn new() -> Self {
            Self {
                capabilities: ModelCapabilities {
                    context_window_tokens: 8192,
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
            }
        }
    }

    #[async_trait]
    impl LLMCore for ErrorMock {
        async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
            self.infer_with_tools(context, &[]).await
        }

        async fn infer_with_tools(
            &self,
            _context: &ContextWindow,
            _tools: &[ToolManifest],
        ) -> Result<InferenceResult, AgentOSError> {
            Err(AgentOSError::LLMError {
                provider: "error-mock".to_string(),
                reason: "Intentional test failure".to_string(),
            })
        }

        async fn infer_stream_with_tools(
            &self,
            _context: &ContextWindow,
            _tools: &[ToolManifest],
            _tx: mpsc::Sender<InferenceEvent>,
        ) -> Result<(), AgentOSError> {
            Err(AgentOSError::LLMError {
                provider: "error-mock".to_string(),
                reason: "Intentional streaming test failure".to_string(),
            })
        }

        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        async fn health_check(&self) -> HealthStatus {
            HealthStatus::Unhealthy {
                reason: "Always unhealthy".to_string(),
            }
        }

        fn provider_name(&self) -> &str {
            "error-mock"
        }

        fn model_name(&self) -> &str {
            "none"
        }
    }

    #[tokio::test]
    async fn test_fallback_first_provider_succeeds() {
        let primary = Arc::new(MockLLMCore::new(vec!["primary response".to_string()]));
        let secondary = Arc::new(MockLLMCore::new(vec!["secondary response".to_string()]));
        let fallback = FallbackAdapter::new(vec![
            primary.clone() as Arc<dyn LLMCore>,
            secondary.clone() as Arc<dyn LLMCore>,
        ])
        .unwrap();

        let ctx = ContextWindow::new(10);
        let result = fallback.infer(&ctx).await.unwrap();
        assert_eq!(result.text, "primary response");
        // Secondary was never called.
        assert_eq!(secondary.call_count(), 0);
    }

    #[tokio::test]
    async fn test_fallback_skips_failing_provider() {
        let primary = Arc::new(ErrorMock::new());
        let secondary = Arc::new(MockLLMCore::new(vec!["fallback response".to_string()]));
        let fallback = FallbackAdapter::new(vec![
            primary as Arc<dyn LLMCore>,
            secondary.clone() as Arc<dyn LLMCore>,
        ])
        .unwrap();

        let ctx = ContextWindow::new(10);
        let result = fallback.infer(&ctx).await.unwrap();
        assert_eq!(result.text, "fallback response");
        assert_eq!(secondary.call_count(), 1);
    }

    #[tokio::test]
    async fn test_fallback_all_fail_returns_error() {
        let fallback = FallbackAdapter::new(vec![
            Arc::new(ErrorMock::new()) as Arc<dyn LLMCore>,
            Arc::new(ErrorMock::new()) as Arc<dyn LLMCore>,
        ])
        .unwrap();

        let ctx = ContextWindow::new(10);
        let result = fallback.infer(&ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Intentional test failure"));
    }

    #[tokio::test]
    async fn test_fallback_health_check_healthy_when_any_healthy() {
        let fallback = FallbackAdapter::new(vec![
            Arc::new(ErrorMock::new()) as Arc<dyn LLMCore>,
            Arc::new(MockLLMCore::new(vec![])) as Arc<dyn LLMCore>,
        ])
        .unwrap();
        assert!(fallback.health_check().await.is_healthy());
    }

    #[tokio::test]
    async fn test_fallback_health_check_unhealthy_when_all_unhealthy() {
        let fallback = FallbackAdapter::new(vec![
            Arc::new(ErrorMock::new()) as Arc<dyn LLMCore>,
            Arc::new(ErrorMock::new()) as Arc<dyn LLMCore>,
        ])
        .unwrap();
        assert!(!fallback.health_check().await.is_healthy());
    }

    #[tokio::test]
    async fn test_fallback_provider_name_and_model() {
        let primary = Arc::new(MockLLMCore::new(vec![]));
        let fallback = FallbackAdapter::new(vec![primary as Arc<dyn LLMCore>]).unwrap();
        assert_eq!(fallback.provider_name(), "fallback");
        assert_eq!(fallback.model_name(), "mock-model");
    }

    #[tokio::test]
    async fn test_fallback_new_empty_providers_returns_error() {
        let result = FallbackAdapter::new(vec![]);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fallback_streaming_skips_failing_provider() {
        let primary = Arc::new(ErrorMock::new());
        let secondary = Arc::new(MockLLMCore::new(vec!["streamed".to_string()]));
        let fallback = FallbackAdapter::new(vec![
            primary as Arc<dyn LLMCore>,
            secondary.clone() as Arc<dyn LLMCore>,
        ])
        .unwrap();

        let ctx = ContextWindow::new(10);
        let (tx, mut rx) = mpsc::channel(64);
        fallback
            .infer_stream_with_tools(&ctx, &[], tx)
            .await
            .unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        // Events come from the secondary (successful) provider only.
        assert!(!events.is_empty());
        assert!(matches!(events.last(), Some(InferenceEvent::Done(_))));
        assert_eq!(secondary.call_count(), 1);
    }
}
