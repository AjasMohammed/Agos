use crate::types::{
    HealthStatus, InferenceEvent, InferenceOptions, InferenceResult, ModelCapabilities,
};
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
        let content_chars: usize = context
            .active_entries()
            .iter()
            .map(|e| e.content.len())
            .sum();
        let tool_chars: usize = tools
            .iter()
            .map(|t| t.manifest.description.len() + t.manifest.name.len() + 100)
            .sum();
        ((content_chars + tool_chars) as f64 / 4.0).ceil() as u64
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
        ContextCategory, ContextEntry, ContextPartition, ContextRole, ContextWindow,
    };

    fn make_entry(role: ContextRole, content: &str) -> ContextEntry {
        ContextEntry {
            role,
            content: content.to_string(),
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
                name: "file-reader".to_string(),         // 11 chars
                description: "Reads a file".to_string(), // 12 chars
                version: "1.0.0".to_string(),
                author: "test".to_string(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier: TrustTier::Core,
            },
            capabilities_required: ToolCapabilities {
                permissions: vec![],
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "Any".to_string(),
                output: "Any".to_string(),
            },
            input_schema: None,
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
        };
        // name(11) + description(12) + overhead(100) = 123 chars → ceil(123/4) = 31
        let estimate = mock.estimate_tokens(&ctx, &[manifest]);
        assert_eq!(estimate, 31);
    }
}
