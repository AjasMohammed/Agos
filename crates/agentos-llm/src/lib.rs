pub mod anthropic;
pub mod catalog;
pub mod claude_code;
pub mod custom;
pub mod fallback;
pub mod gemini;
pub mod media;
pub mod mock;
pub mod ollama;
pub mod openai;
pub mod retry;
pub mod session;
pub mod streaming_helpers;
pub mod tool_helpers;
pub mod traits;
pub mod types;

pub use anthropic::AnthropicCore;
pub use catalog::{CatalogEntry, ProviderCatalog};
pub use claude_code::ClaudeCodeCore;
pub use custom::CustomCore;
pub use fallback::FallbackAdapter;
pub use gemini::GeminiCore;
pub use media::{ImageResolver, NoopImageResolver};
pub use mock::{MockCallMethod, MockCallRecord, MockLLMCore, MockResponse};
pub use ollama::OllamaCore;
pub use openai::OpenAICore;
pub use retry::{CircuitBreaker, RetryPolicy};
pub use session::{ClaudeSessionLookup, SessionState};
pub use traits::LLMCore;
pub use types::{
    calculate_inference_cost, default_pricing_table, lookup_pricing, parse_uncertainty,
    HealthStatus, InferenceCost, InferenceEvent, InferenceOptions, InferenceResult,
    InferenceToolCall, ModelCapabilities, ModelPricing, PromptCacheTtl, StopReason, TokenUsage,
    ToolChoice,
};

/// Assistant turn announcing native tool calls `(id, tool_name)`. A tool result
/// only reaches the wire after the call it answers (`ContextWindow::wire_entries`).
#[cfg(test)]
pub(crate) fn tool_call_turn(calls: &[(&str, &str)]) -> agentos_types::ContextEntry {
    let mut e = agentos_types::ContextEntry::from_text(agentos_types::ContextRole::Assistant, "");
    e.metadata = Some(agentos_types::ContextMetadata {
        tool_name: None,
        tool_id: None,
        intent_id: None,
        tokens_estimated: None,
        tool_call_id: None,
        assistant_tool_calls: Some(serde_json::Value::Array(
            calls
                .iter()
                .map(|(id, name)| serde_json::json!({"id": id, "tool_name": name}))
                .collect(),
        )),
    });
    e
}
