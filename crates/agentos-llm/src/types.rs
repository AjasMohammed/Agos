use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    pub text: String,
    pub tokens_used: TokenUsage,
    pub model: String,
    pub duration_ms: u64,
    /// Parsed uncertainty declaration from the LLM response, if present.
    /// Populated when the response contains an `[UNCERTAINTY]` block.
    #[serde(default)]
    pub uncertainty: Option<UncertaintyDeclaration>,
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
}
