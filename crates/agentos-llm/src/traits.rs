use crate::types::{HealthStatus, InferenceEvent, InferenceResult, ModelCapabilities};
use agentos_types::*;
use async_trait::async_trait;
use tokio::sync::mpsc;

#[async_trait]
pub trait LLMCore: Send + Sync {
    /// Send a context window to the LLM and get a complete response.
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError>;

    /// Send a context window plus tool manifests to the LLM and get a complete response.
    ///
    /// Default behavior falls back to `infer()` so adapters without native tool APIs
    /// remain compatible.
    async fn infer_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
    ) -> Result<InferenceResult, AgentOSError> {
        let _ = tools;
        self.infer(context).await
    }

    /// Streaming inference — sends tokens incrementally as they are generated.
    ///
    /// The default implementation falls back to `infer()` and sends the full result
    /// as a single token event followed by a Done event. Adapters that support native
    /// streaming (Ollama, OpenAI SSE, etc.) should override this for real incremental output.
    ///
    /// **Note:** This default calls `infer()` directly — it does NOT delegate through
    /// `infer_stream_with_tools` to avoid fragile delegation chains. Implementors
    /// that override `infer` to delegate to `infer_with_tools` (as OpenAI, Anthropic,
    /// and Gemini do) are safe because `infer_with_tools` terminates.
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

    /// Streaming inference with tool definitions.
    ///
    /// Default falls back to `infer_with_tools()` and emits the result as a
    /// single token + Done pair. Adapters with native streaming should override.
    async fn infer_stream_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        match self.infer_with_tools(context, tools).await {
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
