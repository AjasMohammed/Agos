use crate::types::{
    HealthStatus, InferenceEvent, InferenceOptions, InferenceResult, ModelCapabilities,
};
use agentos_types::*;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Hard per-request wall-clock budget for one inference, shared by every
/// adapter that does not take the value from config or a catalog entry.
///
/// This is the *only* hard deadline on an inference. Reasoning models on hosted
/// gateways routinely run for minutes with nothing on the wire, so a tight
/// transport cap kills healthy turns; the kernel's inference watchdog is an
/// advisory prompt, not a second deadline (see `inference_watchdog_secs`).
pub const DEFAULT_INFERENCE_TIMEOUT_SECS: u64 = 600;

#[async_trait]
pub trait LLMCore: Send + Sync {
    /// Whether the adapter primarily uses provider-native tool-calling
    /// protocol (tool_calls/tool_use/functionCall) instead of relying on
    /// JSON-in-markdown tool instructions in the system prompt.
    fn supports_native_tool_calling(&self) -> bool {
        false
    }

    /// Whether the adapter can take the whole catalogue with the deferred tail
    /// flagged (`InferenceOptions::deferred_tools_from`) and expand
    /// `tool_reference`s itself (Anthropic tool search). When true the kernel
    /// sends every tool and skips its own re-arm path.
    fn supports_deferred_tools(&self) -> bool {
        false
    }

    /// Whether the adapter reaches AgentOS tools through the MCP *gateway*
    /// (the 4 `mcp__agentos__*` meta-tools) instead of receiving the real
    /// kebab-case tool array natively.
    ///
    /// This is a distinct axis from [`Self::supports_native_tool_calling`],
    /// which is a *protocol* proxy: an Anthropic or OpenAI adapter is native
    /// but not gatewayed, and must never be told it only has the 4 wrappers.
    fn uses_tool_gateway(&self) -> bool {
        false
    }

    /// Seconds the kernel waits on a single `infer*` call before opening the
    /// inference user-gate (watchdog).
    ///
    /// This is an *advisory* threshold: it asks an attached operator whether to
    /// keep waiting. When nobody answers, the inference keeps running and the
    /// adapter's transport timeout remains the only hard bound
    /// (`DEFAULT_INFERENCE_TIMEOUT_SECS` for adapters that do not configure
    /// their own). Adapters whose one `infer` call encompasses an entire
    /// internal tool loop (e.g. the claude-code MCP subprocess, which discovers
    /// + invokes + reasons in one shot) return a larger value so operators are
    /// not prompted about turns that are normal for them.
    fn inference_watchdog_secs(&self) -> u64 {
        120
    }

    /// Absolute ceiling (seconds) the kernel enforces on one `infer*` call.
    ///
    /// The adapter's own transport timeout is supposed to be this bound, but a
    /// hung upstream is exactly the situation in which a transport timeout is
    /// least trustworthy (observed: an inference against a multiplexed HTTP/2
    /// gateway outliving its 600s `reqwest` deadline). The kernel therefore
    /// re-asserts the adapter's declared budget itself, so no adapter or HTTP
    /// stack can strand a task indefinitely. Report the real budget here —
    /// returning a value below what the adapter needs kills healthy turns.
    fn inference_hard_timeout_secs(&self) -> u64 {
        DEFAULT_INFERENCE_TIMEOUT_SECS
    }

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

    /// Inference with full options control. This is the primary method for
    /// agentic workflows. The default implementation ignores options and
    /// delegates to `infer_with_tools()`.
    async fn infer_with_options(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        options: &InferenceOptions,
    ) -> Result<InferenceResult, AgentOSError> {
        let _ = options;
        self.infer_with_tools(context, tools).await
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

    /// Estimate the token count for a context window + tools.
    /// Used for pre-flight overflow detection before sending a request.
    /// The default implementation uses a characters/4 heuristic; adapters with
    /// access to a real tokenizer can override this for higher accuracy.
    fn estimate_tokens(&self, context: &ContextWindow, tools: &[ToolManifest]) -> u64 {
        let mut content_chars: usize = 0;
        let mut image_units: u64 = 0;
        for e in context.active_entries() {
            for p in &e.parts {
                match p {
                    agentos_types::ContentPart::Text { text } => content_chars += text.len(),
                    agentos_types::ContentPart::Image { .. } => image_units += 1,
                }
            }
        }
        let tool_chars: usize = tools
            .iter()
            .map(|t| t.manifest.description.len() + t.manifest.name.len() + 100)
            .sum();
        let base = ((content_chars + tool_chars) as f64 / 4.0).ceil() as u64;
        base.saturating_add(image_units.saturating_mul(1500))
    }

    /// Whether this adapter emits native image blocks (`supports_images` cap).
    fn supports_images(&self) -> bool {
        self.capabilities().supports_images
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockLLMCore;
    use agentos_types::tool::{
        ToolCapabilities, ToolExecutor, ToolInfo, ToolOutputs, ToolSandbox, ToolSchema,
    };
    use agentos_types::{
        ContentPart, ContextCategory, ContextEntry, ContextPartition, ContextRole, ContextWindow,
    };

    fn make_entry(role: ContextRole, content: &str) -> ContextEntry {
        ContextEntry {
            role,
            parts: vec![ContentPart::Text {
                text: content.to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::default(),
            is_summary: false,
        }
    }

    #[test]
    fn test_estimate_tokens_heuristic() {
        let mock = MockLLMCore::new(vec!["hello".to_string()]);
        let mut ctx = ContextWindow::new(100);
        // 400 chars of content → 100 tokens (chars / 4, exact)
        ctx.push(make_entry(ContextRole::User, &"a".repeat(400)));
        let estimate = mock.estimate_tokens(&ctx, &[]);
        assert_eq!(estimate, 100);
    }

    #[test]
    fn test_estimate_tokens_includes_tools() {
        let mock = MockLLMCore::new(vec!["hello".to_string()]);
        let ctx = ContextWindow::new(100);
        let manifest = ToolManifest {
            manifest: ToolInfo {
                category: None,
                search_hints: vec![],
                name: "file-reader".to_string(),         // 11 chars
                description: "Reads a file".to_string(), // 12 chars
                version: "1.0.0".to_string(),
                author: "test".to_string(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier: TrustTier::Core,
                tags: None,
                capability_tags: vec![],
                group: String::new(),
            },
            capabilities_required: ToolCapabilities {
                permissions: vec![],
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "Any".to_string(),
                output: "Any".to_string(),
            },
            payload_schema: None,
            examples: vec![],
            sandbox: ToolSandbox {
                network: false,
                fs_write: false,
                gpu: false,
                max_memory_mb: 64,
                max_cpu_ms: 1000,
                syscalls: vec![],
                weight: None,
            },
            executor: ToolExecutor::default(),
            fallbacks: vec![],
            risk_class: Default::default(),
            risk_class_by_action: Default::default(),
            usage_hints: None,
            tags: vec![],
        };
        // name(11) + description(12) + overhead(100) = 123 chars → ceil(123/4) = 31
        let estimate = mock.estimate_tokens(&ctx, &[manifest]);
        assert_eq!(estimate, 31);
    }
}
