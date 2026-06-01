//! Claude Code (subprocess) LLM adapter.
//!
//! Runs the locally-installed `claude` CLI in headless print mode
//! (`claude -p ... --output-format json`) as a pure text reasoning core, so an
//! AgentOS agent can run on a Claude Code / Claude.ai subscription with **no
//! Anthropic API key**.
//!
//! Built-in Claude Code tools are disabled (`--allowed-tools ""`), so the model
//! does not start its own nested agent loop — it just reasons and emits AgentOS
//! tool-call JSON as text. `supports_native_tool_calling()` returns `false`, so
//! the kernel injects the `## Tools` JSON instructions into the system prompt
//! and recovers tool calls from `InferenceResult.text` (the same path as the
//! Ollama adapter).
//!
//! # Cost
//! Inference consumes the user's **Claude Code subscription quota / rate
//! limits**, not metered API credits. A subprocess is spawned per inference
//! step, so latency is higher than a direct HTTP API call. v1 is non-streaming.

use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::traits::LLMCore;
use crate::types::{HealthStatus, InferenceResult, ModelCapabilities, StopReason, TokenUsage};
use agentos_types::{AgentOSError, ContextRole, ContextWindow};

/// Default model when none (or "default") is specified. Matches the `claude`
/// CLI's own default tier.
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
/// Per-inference subprocess timeout. Generous because the CLI does its own
/// startup + context-cache work before responding.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_000;

/// LLM adapter that delegates inference to the local `claude` CLI.
pub struct ClaudeCodeCore {
    /// Binary to invoke (default `"claude"`; overridable for testing / custom paths).
    binary: String,
    /// Model passed via `--model`.
    model: String,
    /// Per-call subprocess timeout.
    timeout: Duration,
    capabilities: ModelCapabilities,
}

impl ClaudeCodeCore {
    /// Build an adapter for the given model. Empty or `"default"` selects
    /// [`DEFAULT_MODEL`].
    pub fn new(model: impl Into<String>) -> Self {
        let model = model.into();
        let model = if model.is_empty() || model == "default" {
            DEFAULT_MODEL.to_string()
        } else {
            model
        };
        Self {
            binary: "claude".to_string(),
            model,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            capabilities: ModelCapabilities {
                context_window_tokens: DEFAULT_CONTEXT_WINDOW,
                supports_images: false,
                // We deliberately use AgentOS's JSON-in-markdown tool path, not
                // a native tool API — see module docs.
                supports_tool_calling: false,
                supports_json_mode: false,
                max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
                supports_streaming: false,
                supports_parallel_tools: false,
                supports_prompt_caching: false,
                supports_thinking: false,
                supports_structured_output: false,
            },
        }
    }

    /// Override the binary path/name (e.g. an absolute path to `claude`).
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Override the per-inference subprocess timeout.
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout = Duration::from_secs(secs);
        self
    }

    fn err(&self, reason: impl Into<String>) -> AgentOSError {
        AgentOSError::LLMError {
            provider: "claude-code".to_string(),
            reason: reason.into(),
        }
    }

    /// Split a [`ContextWindow`] into `(system_prompt, conversation_text)`.
    ///
    /// System entries are joined into the `--system-prompt` value (this carries
    /// AgentOS's `## Tools` JSON instructions). All other roles are flattened
    /// into the prompt argument with role markers, mirroring
    /// `ollama.rs::context_to_messages`.
    fn flatten_context(context: &ContextWindow) -> (String, String) {
        let mut system = String::new();
        let mut convo = String::new();
        for entry in context.active_entries() {
            let text = entry.text();
            match entry.role {
                ContextRole::System => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&text);
                }
                ContextRole::User => {
                    convo.push_str("\n\n## User\n");
                    convo.push_str(&text);
                }
                ContextRole::Assistant => {
                    convo.push_str("\n\n## Assistant\n");
                    convo.push_str(&text);
                }
                ContextRole::ToolResult => {
                    convo.push_str("\n\n## Tool Result\n");
                    convo.push_str(&text);
                }
            }
        }
        (system.trim().to_string(), convo.trim().to_string())
    }

    /// Parse the `--output-format json` payload from the CLI into an
    /// [`InferenceResult`]. Pure (no I/O) so it is unit-testable.
    fn parse_cli_json(
        &self,
        stdout: &str,
        fallback_duration_ms: u64,
    ) -> Result<InferenceResult, AgentOSError> {
        let parsed: ClaudeCliResult = serde_json::from_str(stdout.trim())
            .map_err(|e| self.err(format!("invalid JSON from claude CLI: {e}")))?;

        if parsed.is_error {
            let msg = parsed
                .result
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "claude CLI reported is_error=true".to_string());
            return Err(self.err(format!("claude CLI error: {msg}")));
        }

        let text = parsed
            .result
            .ok_or_else(|| self.err("claude CLI response missing 'result' field"))?;

        let usage = parsed.usage.unwrap_or_default();
        // Count cached + freshly-created prompt tokens as prompt tokens so cost
        // attribution and overflow heuristics see the true input size.
        let prompt_tokens = usage
            .input_tokens
            .saturating_add(usage.cache_read_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens);
        let completion_tokens = usage.output_tokens;

        let stop_reason = match parsed.stop_reason.as_deref() {
            None | Some("end_turn") => StopReason::EndTurn,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("stop_sequence") => StopReason::StopSequence,
            Some("tool_use") => StopReason::ToolUse,
            Some(other) => StopReason::Other(other.to_string()),
        };

        Ok(InferenceResult {
            text,
            tokens_used: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
            },
            model: self.model.clone(),
            duration_ms: parsed.duration_ms.unwrap_or(fallback_duration_ms),
            tool_calls: Vec::new(),
            uncertainty: None,
            stop_reason,
            cost: None,
            cached_tokens: usage.cache_read_input_tokens,
        })
    }
}

#[async_trait]
impl LLMCore for ClaudeCodeCore {
    fn supports_native_tool_calling(&self) -> bool {
        false
    }

    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let (system, convo) = Self::flatten_context(context);
        // `claude -p` requires a non-empty prompt; fall back to a nudge.
        let prompt = if convo.is_empty() {
            "Continue.".to_string()
        } else {
            convo
        };

        let mut cmd = Command::new(&self.binary);
        cmd.arg("-p")
            .arg(&prompt)
            .arg("--output-format")
            .arg("json")
            .arg("--model")
            .arg(&self.model)
            // Disable every built-in tool so Claude acts as a pure reasoning
            // core and emits AgentOS tool-call JSON instead of running its own
            // tool loop.
            .arg("--allowed-tools")
            .arg("");
        if !system.is_empty() {
            cmd.arg("--system-prompt").arg(&system);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let start = Instant::now();
        let output = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| {
                self.err(format!(
                    "claude CLI timed out after {}s",
                    self.timeout.as_secs()
                ))
            })?
            .map_err(|e| self.err(format!("failed to spawn '{}': {e}", self.binary)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(self.err(format!(
                "claude CLI exited with {}: {}",
                output.status,
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        self.parse_cli_json(&stdout, start.elapsed().as_millis() as u64)
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> HealthStatus {
        // `claude --version` is fast and free (no quota); a clean exit means the
        // CLI is installed and runnable. We do not probe auth here to avoid
        // burning subscription quota on every connect.
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(out)) if out.status.success() => HealthStatus::Healthy,
            Ok(Ok(out)) => HealthStatus::Unhealthy {
                reason: format!("`{} --version` exited with {}", self.binary, out.status),
            },
            Ok(Err(e)) => HealthStatus::Unhealthy {
                reason: format!(
                    "cannot run '{}': {e}. Install Claude Code and run `claude` once to log in.",
                    self.binary
                ),
            },
            Err(_) => HealthStatus::Unhealthy {
                reason: format!("`{} --version` timed out", self.binary),
            },
        }
    }

    fn provider_name(&self) -> &str {
        "claude-code"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

/// Subset of the `claude --output-format json` result object we consume.
#[derive(Debug, Deserialize)]
struct ClaudeCliResult {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    usage: Option<ClaudeCliUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeCliUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{ContextEntry, ContextWindow};

    fn entry(role: ContextRole, text: &str) -> ContextEntry {
        ContextEntry::from_text(role, text.to_string())
    }

    #[test]
    fn parse_success_sums_cached_tokens_into_prompt() {
        let core = ClaudeCodeCore::new("claude-sonnet-4-6");
        let json = r#"{
            "type":"result","subtype":"success","is_error":false,
            "result":"hello world","stop_reason":"end_turn","duration_ms":1892,
            "usage":{"input_tokens":3,"output_tokens":4,
                     "cache_read_input_tokens":100,"cache_creation_input_tokens":20}
        }"#;
        let r = core.parse_cli_json(json, 0).expect("parse ok");
        assert_eq!(r.text, "hello world");
        assert_eq!(r.stop_reason, StopReason::EndTurn);
        assert_eq!(r.tokens_used.prompt_tokens, 123); // 3 + 100 + 20
        assert_eq!(r.tokens_used.completion_tokens, 4);
        assert_eq!(r.tokens_used.total_tokens, 127);
        assert_eq!(r.cached_tokens, 100);
        assert_eq!(r.duration_ms, 1892);
        assert!(r.tool_calls.is_empty());
    }

    #[test]
    fn parse_is_error_returns_err() {
        let core = ClaudeCodeCore::new("default");
        let json = r#"{"type":"result","is_error":true,"result":"rate limited"}"#;
        let err = core.parse_cli_json(json, 0).unwrap_err();
        assert!(format!("{err}").contains("rate limited"));
    }

    #[test]
    fn parse_malformed_returns_err() {
        let core = ClaudeCodeCore::new("default");
        assert!(core.parse_cli_json("not json", 0).is_err());
    }

    #[test]
    fn parse_maps_stop_reasons() {
        let core = ClaudeCodeCore::new("default");
        let mk = |sr: &str| format!(r#"{{"is_error":false,"result":"x","stop_reason":"{sr}"}}"#);
        assert_eq!(
            core.parse_cli_json(&mk("max_tokens"), 0)
                .unwrap()
                .stop_reason,
            StopReason::MaxTokens
        );
        assert_eq!(
            core.parse_cli_json(&mk("weird"), 0).unwrap().stop_reason,
            StopReason::Other("weird".to_string())
        );
    }

    #[test]
    fn parse_missing_duration_uses_fallback() {
        let core = ClaudeCodeCore::new("default");
        let json = r#"{"is_error":false,"result":"x"}"#;
        assert_eq!(core.parse_cli_json(json, 555).unwrap().duration_ms, 555);
    }

    #[test]
    fn default_model_applied() {
        assert_eq!(ClaudeCodeCore::new("").model_name(), DEFAULT_MODEL);
        assert_eq!(ClaudeCodeCore::new("default").model_name(), DEFAULT_MODEL);
        assert_eq!(
            ClaudeCodeCore::new("claude-opus-4-8").model_name(),
            "claude-opus-4-8"
        );
    }

    #[test]
    fn flatten_splits_system_and_marks_roles() {
        let mut ctx = ContextWindow::new(10_000);
        ctx.push(entry(ContextRole::System, "SYS-A"));
        ctx.push(entry(ContextRole::System, "SYS-B"));
        ctx.push(entry(ContextRole::User, "hi there"));
        ctx.push(entry(ContextRole::Assistant, "hello"));
        ctx.push(entry(ContextRole::ToolResult, "tool out"));
        let (system, convo) = ClaudeCodeCore::flatten_context(&ctx);
        assert!(system.contains("SYS-A") && system.contains("SYS-B"));
        assert!(!convo.contains("SYS-A"));
        assert!(convo.contains("## User") && convo.contains("hi there"));
        assert!(convo.contains("## Assistant") && convo.contains("hello"));
        assert!(convo.contains("## Tool Result") && convo.contains("tool out"));
    }

    #[test]
    fn provider_name_is_claude_code() {
        assert_eq!(
            ClaudeCodeCore::new("default").provider_name(),
            "claude-code"
        );
    }
}
