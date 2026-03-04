use crate::types::{InferenceResult, ModelCapabilities};
use agentos_types::*;
use async_trait::async_trait;

#[async_trait]
pub trait LLMCore: Send + Sync {
    /// Send a context window to the LLM and get a response.
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError>;

    /// Get the model's capabilities (context window size, etc.)
    fn capabilities(&self) -> &ModelCapabilities;

    /// Check if the LLM backend is reachable and healthy.
    async fn health_check(&self) -> bool;

    /// Get the provider name (for display/logging).
    fn provider_name(&self) -> &str;

    /// Get the model name.
    fn model_name(&self) -> &str;
}
