use serde::{Deserialize, Serialize};

/// Why the model stopped generating.
/// Used by the kernel to decide whether to continue the agentic loop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// Model finished naturally (OpenAI: "stop", Anthropic: "end_turn", Gemini: "STOP").
    #[default]
    EndTurn,
    /// Model wants to call one or more tools (OpenAI: "tool_calls", Anthropic: "tool_use", Gemini: "FUNCTION_CALL").
    ToolUse,
    /// Model hit the max_tokens limit and was truncated.
    MaxTokens,
    /// Content was filtered by the provider's safety system.
    ContentFilter,
    /// A stop sequence was matched.
    StopSequence,
    /// Unknown or provider-specific reason.
    Other(String),
}

/// Tool choice strategy for inference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolChoice {
    /// Let the model decide (default).
    Auto,
    /// Model must not call any tools.
    None,
    /// Model must call at least one tool.
    Required,
    /// Model must call this specific tool.
    Specific(String),
}

/// Per-inference configuration options.
/// Passed to `infer_with_options` to control behavior for a single call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceOptions {
    /// Tool choice strategy. None = provider default (usually "auto").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether to request streaming. None = non-streaming.
    #[serde(default)]
    pub stream: bool,
    /// Temperature override for this call. None = model default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Max output tokens override. None = adapter default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Request structured JSON output.
    #[serde(default)]
    pub json_mode: bool,
    /// Seed for reproducible output (OpenAI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    pub text: String,
    pub tokens_used: TokenUsage,
    pub model: String,
    pub duration_ms: u64,
    /// Structured tool calls emitted by the model, if any.
    /// Populated by adapters that support native function/tool calling APIs.
    #[serde(default)]
    pub tool_calls: Vec<InferenceToolCall>,
    /// Parsed uncertainty declaration from the LLM response, if present.
    /// Populated when the response contains an `[UNCERTAINTY]` block.
    #[serde(default)]
    pub uncertainty: Option<UncertaintyDeclaration>,
    /// Why the model stopped generating. Drives kernel agentic loop control.
    #[serde(default)]
    pub stop_reason: StopReason,
    /// Cost of this inference call, if computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<InferenceCost>,
    /// Number of prompt tokens that were cache hits (Anthropic/OpenAI).
    #[serde(default)]
    pub cached_tokens: u64,
}

/// Structured tool call emitted by an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceToolCall {
    /// Provider-native tool call identifier (e.g., OpenAI `call_xxx`), if present.
    #[serde(default)]
    pub id: Option<String>,
    /// Tool/function name requested by the model.
    pub tool_name: String,
    /// AgentOS intent type string (e.g. read/write/execute/query).
    pub intent_type: String,
    /// Parsed JSON arguments for the tool invocation.
    pub payload: serde_json::Value,
}

/// Structured declaration of uncertainty from an LLM response.
/// Agents can emit `[UNCERTAINTY]` blocks in their responses, which the
/// kernel extracts into this typed struct for downstream decision-making.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UncertaintyDeclaration {
    /// Overall confidence level from 0.0 (no confidence) to 1.0 (fully confident).
    pub overall_confidence: f32,
    /// Specific claims the agent is uncertain about.
    pub uncertain_claims: Vec<String>,
    /// Suggested verification action, if any.
    pub suggested_verification: Option<String>,
}

/// Events emitted during streaming inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InferenceEvent {
    /// A chunk of generated text (one or more tokens).
    Token(String),
    /// A tool call has started (name known, arguments streaming).
    ToolCallStart {
        index: usize,
        id: Option<String>,
        tool_name: String,
    },
    /// A chunk of tool call arguments (for streaming accumulation).
    ToolCallDelta {
        index: usize,
        arguments_chunk: String,
    },
    /// A tool call is fully assembled and ready for execution.
    ToolCallComplete(InferenceToolCall),
    /// Token usage update (may arrive mid-stream or at end).
    Usage(TokenUsage),
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

/// Pricing per 1K tokens for a specific model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub provider: String,
    pub model: String,
    pub input_per_1k: f64,
    pub output_per_1k: f64,
}

/// Cost calculated for a single inference call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceCost {
    pub input_cost_usd: f64,
    pub output_cost_usd: f64,
    pub total_cost_usd: f64,
}

/// Built-in pricing table for known models (USD per 1K tokens).
/// Updated as of March 2026. Users can override via config.
pub fn default_pricing_table() -> Vec<ModelPricing> {
    vec![
        // Anthropic
        ModelPricing {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            input_per_1k: 0.003,
            output_per_1k: 0.015,
        },
        ModelPricing {
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            input_per_1k: 0.015,
            output_per_1k: 0.075,
        },
        ModelPricing {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            input_per_1k: 0.0008,
            output_per_1k: 0.004,
        },
        // OpenAI
        ModelPricing {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            input_per_1k: 0.0025,
            output_per_1k: 0.01,
        },
        ModelPricing {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            input_per_1k: 0.00015,
            output_per_1k: 0.0006,
        },
        // Google
        ModelPricing {
            provider: "gemini".into(),
            model: "gemini-2.0-flash".into(),
            input_per_1k: 0.0001,
            output_per_1k: 0.0004,
        },
        ModelPricing {
            provider: "gemini".into(),
            model: "gemini-2.5-pro".into(),
            input_per_1k: 0.00125,
            output_per_1k: 0.01,
        },
        // Ollama (local — free)
        ModelPricing {
            provider: "ollama".into(),
            model: "*".into(),
            input_per_1k: 0.0,
            output_per_1k: 0.0,
        },
    ]
}

/// Calculate the cost of an inference call.
pub fn calculate_inference_cost(usage: &TokenUsage, pricing: &ModelPricing) -> InferenceCost {
    let input_cost = (usage.prompt_tokens as f64 / 1000.0) * pricing.input_per_1k;
    let output_cost = (usage.completion_tokens as f64 / 1000.0) * pricing.output_per_1k;
    InferenceCost {
        input_cost_usd: input_cost,
        output_cost_usd: output_cost,
        total_cost_usd: input_cost + output_cost,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub context_window_tokens: u64,
    pub supports_images: bool,
    pub supports_tool_calling: bool,
    pub supports_json_mode: bool,
    /// Maximum tokens the model will generate per response.
    /// Set from config at adapter construction time; 0 means not specified.
    #[serde(default)]
    pub max_output_tokens: u64,
    /// Whether the adapter implements real streaming (not fake fallback).
    #[serde(default)]
    pub supports_streaming: bool,
    /// Whether the model can emit multiple tool calls in one turn.
    #[serde(default)]
    pub supports_parallel_tools: bool,
    /// Whether prompt caching is available (reduces cost on repeated context).
    #[serde(default)]
    pub supports_prompt_caching: bool,
    /// Whether the model supports extended thinking / chain-of-thought.
    #[serde(default)]
    pub supports_thinking: bool,
    /// Whether structured JSON output mode is enforced (not just JSON in instructions).
    #[serde(default)]
    pub supports_structured_output: bool,
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

/// Parse an `[UNCERTAINTY]...[/UNCERTAINTY]` block from LLM response text.
///
/// Expected format inside the block:
/// ```text
/// confidence: 0.6
/// claims: claim1; claim2; claim3
/// verify: suggested verification action
/// ```
pub fn parse_uncertainty(text: &str) -> Option<UncertaintyDeclaration> {
    let start = text.find("[UNCERTAINTY]")?;
    let end = text.find("[/UNCERTAINTY]")?;
    if end <= start {
        return None;
    }
    let block = &text[start + "[UNCERTAINTY]".len()..end].trim();

    let mut confidence = 0.5f32;
    let mut claims = Vec::new();
    let mut verification = None;

    for line in block.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("confidence:") {
            if let Ok(c) = val.trim().parse::<f32>() {
                confidence = c.clamp(0.0, 1.0);
            }
        } else if let Some(val) = line.strip_prefix("claims:") {
            claims = val
                .split(';')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        } else if let Some(val) = line.strip_prefix("verify:") {
            let v = val.trim().to_string();
            if !v.is_empty() {
                verification = Some(v);
            }
        }
    }

    Some(UncertaintyDeclaration {
        overall_confidence: confidence,
        uncertain_claims: claims,
        suggested_verification: verification,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uncertainty_block() {
        let text = r#"Here is my analysis.
[UNCERTAINTY]
confidence: 0.7
claims: the database might not support this; performance could degrade
verify: Run a benchmark test
[/UNCERTAINTY]
And here is the rest of the response."#;

        let result = parse_uncertainty(text).unwrap();
        assert!((result.overall_confidence - 0.7).abs() < 0.01);
        assert_eq!(result.uncertain_claims.len(), 2);
        assert_eq!(
            result.suggested_verification.as_deref(),
            Some("Run a benchmark test")
        );
    }

    #[test]
    fn test_parse_uncertainty_missing() {
        assert!(parse_uncertainty("No uncertainty here").is_none());
    }

    #[test]
    fn test_parse_uncertainty_minimal() {
        let text = "[UNCERTAINTY]\nconfidence: 0.3\n[/UNCERTAINTY]";
        let result = parse_uncertainty(text).unwrap();
        assert!((result.overall_confidence - 0.3).abs() < 0.01);
        assert!(result.uncertain_claims.is_empty());
        assert!(result.suggested_verification.is_none());
    }

    #[test]
    fn test_stop_reason_default() {
        assert_eq!(StopReason::default(), StopReason::EndTurn);
    }

    #[test]
    fn test_stop_reason_equality() {
        assert_eq!(StopReason::ToolUse, StopReason::ToolUse);
        assert_ne!(StopReason::EndTurn, StopReason::ToolUse);
        assert_eq!(
            StopReason::Other("custom".into()),
            StopReason::Other("custom".into())
        );
        assert_ne!(StopReason::Other("a".into()), StopReason::Other("b".into()));
    }

    #[test]
    fn test_stop_reason_serde_roundtrip() {
        let reasons = vec![
            StopReason::EndTurn,
            StopReason::ToolUse,
            StopReason::MaxTokens,
            StopReason::ContentFilter,
            StopReason::StopSequence,
            StopReason::Other("custom_reason".into()),
        ];
        for reason in reasons {
            let json = serde_json::to_string(&reason).unwrap();
            let deserialized: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(reason, deserialized);
        }
    }

    #[test]
    fn test_inference_options_default() {
        let opts = InferenceOptions::default();
        assert!(opts.tool_choice.is_none());
        assert!(!opts.stream);
        assert!(opts.temperature.is_none());
        assert!(opts.max_tokens.is_none());
        assert!(!opts.json_mode);
        assert!(opts.seed.is_none());
    }

    #[test]
    fn test_inference_options_serde_roundtrip() {
        let opts = InferenceOptions {
            tool_choice: Some(ToolChoice::Required),
            stream: true,
            temperature: Some(0.7),
            max_tokens: Some(4096),
            json_mode: true,
            seed: Some(42),
        };
        let json = serde_json::to_string(&opts).unwrap();
        let deserialized: InferenceOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.stream, true);
        assert_eq!(deserialized.max_tokens, Some(4096));
        assert_eq!(deserialized.seed, Some(42));
        assert!(deserialized.json_mode);
    }

    #[test]
    fn test_tool_choice_serde_roundtrip() {
        let choices = vec![
            ToolChoice::Auto,
            ToolChoice::None,
            ToolChoice::Required,
            ToolChoice::Specific("file-reader".into()),
        ];
        for choice in choices {
            let json = serde_json::to_string(&choice).unwrap();
            let deserialized: ToolChoice = serde_json::from_str(&json).unwrap();
            assert_eq!(choice, deserialized);
        }
    }

    #[test]
    fn test_inference_result_new_fields_serde_roundtrip() {
        let result = InferenceResult {
            text: "hello".into(),
            tokens_used: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            model: "test".into(),
            duration_ms: 100,
            tool_calls: vec![],
            uncertainty: None,
            stop_reason: StopReason::ToolUse,
            cost: Some(InferenceCost {
                input_cost_usd: 0.001,
                output_cost_usd: 0.002,
                total_cost_usd: 0.003,
            }),
            cached_tokens: 42,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: InferenceResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.stop_reason, StopReason::ToolUse);
        assert_eq!(deserialized.cached_tokens, 42);
        assert!((deserialized.cost.unwrap().total_cost_usd - 0.003).abs() < 1e-9);
    }

    #[test]
    fn test_cost_attached_to_inference_result() {
        let pricing = ModelPricing {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            input_per_1k: 0.003,
            output_per_1k: 0.015,
        };
        let usage = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };
        let cost = calculate_inference_cost(&usage, &pricing);
        // 1000 tokens * $0.003/1k = $0.003 input
        // 500 tokens * $0.015/1k = $0.0075 output
        assert!((cost.input_cost_usd - 0.003).abs() < 1e-9);
        assert!((cost.output_cost_usd - 0.0075).abs() < 1e-9);
        assert!((cost.total_cost_usd - 0.0105).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_lookup_fallback_zero_cost() {
        // A provider not in the table should yield zero-cost from a custom fallback.
        let pricing = ModelPricing {
            provider: "unknown-provider".to_string(),
            model: "some-model".to_string(),
            input_per_1k: 0.0,
            output_per_1k: 0.0,
        };
        let usage = TokenUsage {
            prompt_tokens: 10_000,
            completion_tokens: 5_000,
            total_tokens: 15_000,
        };
        let cost = calculate_inference_cost(&usage, &pricing);
        assert_eq!(cost.total_cost_usd, 0.0);
    }

    #[test]
    fn test_inference_result_backward_compat_deserialization() {
        // JSON without new fields should still deserialize (serde(default))
        let json = r#"{
            "text": "hi",
            "tokens_used": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            "model": "old",
            "duration_ms": 50,
            "tool_calls": []
        }"#;
        let result: InferenceResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.stop_reason, StopReason::EndTurn);
        assert!(result.cost.is_none());
        assert_eq!(result.cached_tokens, 0);
    }
}
