use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    pub text: String,
    pub tokens_used: TokenUsage,
    pub model: String,
    pub duration_ms: u64,
}

/// Events emitted during streaming inference.
#[derive(Debug, Clone)]
pub enum InferenceEvent {
    /// A chunk of generated text (one or more tokens).
    Token(String),
    /// The final result with complete text and usage statistics.
    Done(InferenceResult),
    /// An error occurred during generation (string representation since AgentOSError is not Clone).
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub context_window_tokens: u64,
    pub supports_images: bool,
    pub supports_tool_calling: bool,
    pub supports_json_mode: bool,
}

/// Health status of an LLM backend, providing richer diagnostics than a bare `bool`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    Degraded { reason: String },
    Unhealthy { reason: String },
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy | HealthStatus::Degraded { .. })
    }
}
