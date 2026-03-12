use crate::types::{HealthStatus, InferenceEvent, InferenceResult, ModelCapabilities};
use agentos_types::*;
use async_trait::async_trait;
use tokio::sync::mpsc;

#[async_trait]
pub trait LLMCore: Send + Sync {
    /// Send a context window to the LLM and get a complete response.
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError>;

    /// Streaming inference — sends tokens incrementally as they are generated.
    ///
    /// The default implementation falls back to `infer()` and sends the full result
    /// as a single token event followed by a Done event. Adapters that support native
    /// streaming (Ollama, OpenAI SSE, etc.) should override this for real incremental output.
    async fn infer_stream(
        &self,
        context: &ContextWindow,
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        match self.infer(context).await {
            Ok(result) => {
                let _ = tx.send(InferenceEvent::Token(result.text.clone())).await;
                let _ = tx.send(InferenceEvent::Done(result)).await;
                Ok(())
            }
            Err(e) => {
                let _ = tx.send(InferenceEvent::Error(e.to_string())).await;
                Err(e)
            }
        }
    }

    /// Get the model's capabilities (context window size, etc.)
    fn capabilities(&self) -> &ModelCapabilities;

    /// Check if the LLM backend is reachable and healthy.
    async fn health_check(&self) -> HealthStatus;

    /// Get the provider name (for display/logging).
    fn provider_name(&self) -> &str;

    /// Get the model name.
    fn model_name(&self) -> &str;
}
