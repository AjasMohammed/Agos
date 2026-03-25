use crate::traits::LLMCore;
use crate::types::{
    InferenceCost, InferenceEvent, InferenceResult, InferenceToolCall, ModelCapabilities,
    StopReason, TokenUsage,
};
use agentos_types::*;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// A preconfigured response for the mock adapter.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub text: String,
    pub tool_calls: Vec<InferenceToolCall>,
    pub stop_reason: StopReason,
    pub tokens_used: TokenUsage,
}

impl MockResponse {
    /// Create a simple text-only response.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            tokens_used: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        }
    }

    /// Add tool calls (sets stop reason to ToolUse automatically).
    ///
    /// **Note:** This overrides any previously set stop reason. If you need a
    /// non-ToolUse stop reason alongside tool calls, call `with_stop_reason()`
    /// after `with_tool_calls()`.
    pub fn with_tool_calls(mut self, calls: Vec<InferenceToolCall>) -> Self {
        self.stop_reason = StopReason::ToolUse;
        self.tool_calls = calls;
        self
    }

    /// Override the stop reason.
    pub fn with_stop_reason(mut self, reason: StopReason) -> Self {
        self.stop_reason = reason;
        self
    }

    /// Override the token usage.
    pub fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.tokens_used = usage;
        self
    }
}

/// Which LLMCore method was invoked in a recorded call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockCallMethod {
    InferWithTools,
    InferStreamWithTools,
}

/// Record of a single call made to the mock adapter.
#[derive(Debug, Clone)]
pub struct MockCallRecord {
    /// Role and content for each active context entry at call time.
    pub context_entries: Vec<(ContextRole, String)>,
    /// Names of tool manifests passed to the call.
    pub tool_names: Vec<String>,
    /// Which trait method was invoked.
    pub method: MockCallMethod,
}

/// A mock LLM backend for testing.
///
/// Returns preconfigured `MockResponse` values in sequence. Supports tool calls,
/// configurable stop reasons, streaming simulation, and call history recording.
///
/// `call_count()` is derived from `call_history().len()` so both are always
/// consistent even under concurrent access.
pub struct MockLLMCore {
    responses: Mutex<VecDeque<MockResponse>>,
    call_history: Mutex<Vec<MockCallRecord>>,
    capabilities: ModelCapabilities,
}

impl MockLLMCore {
    /// Create with typed `MockResponse` values.
    pub fn with_responses(responses: Vec<MockResponse>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
            call_history: Mutex::new(Vec::new()),
            capabilities: ModelCapabilities {
                context_window_tokens: 8192,
                supports_images: false,
                supports_tool_calling: true,
                supports_json_mode: false,
                max_output_tokens: 4096,
                supports_streaming: true,
                supports_parallel_tools: true,
                supports_prompt_caching: false,
                supports_thinking: false,
                supports_structured_output: false,
            },
        }
    }

    /// Backward-compatible constructor — accepts plain strings.
    ///
    /// Each string becomes a `MockResponse::text(...)` with `StopReason::EndTurn`.
    pub fn new(responses: Vec<String>) -> Self {
        Self::with_responses(responses.into_iter().map(MockResponse::text).collect())
    }

    /// How many times an inference method has been called.
    ///
    /// Derived from `call_history` length so it is always consistent with
    /// `call_history()` even under concurrent access.
    pub fn call_count(&self) -> usize {
        self.call_history.lock().unwrap().len()
    }

    /// Return all call records for assertion in tests.
    pub fn call_history(&self) -> Vec<MockCallRecord> {
        self.call_history.lock().unwrap().clone()
    }

    /// Return the Nth call record (0-indexed).
    pub fn call_at(&self, index: usize) -> Option<MockCallRecord> {
        self.call_history.lock().unwrap().get(index).cloned()
    }

    fn pop_response(&self) -> MockResponse {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| MockResponse::text("No more mock responses"))
    }

    fn record_call(&self, context: &ContextWindow, tools: &[ToolManifest], method: MockCallMethod) {
        let context_entries = context
            .active_entries()
            .iter()
            .map(|e| (e.role, e.content.clone()))
            .collect();
        let tool_names = tools.iter().map(|t| t.manifest.name.clone()).collect();
        self.call_history.lock().unwrap().push(MockCallRecord {
            context_entries,
            tool_names,
            method,
        });
    }

    fn make_result(resp: MockResponse) -> InferenceResult {
        InferenceResult {
            text: resp.text,
            tokens_used: resp.tokens_used,
            model: "mock-model".to_string(),
            duration_ms: 1,
            tool_calls: resp.tool_calls,
            uncertainty: None,
            stop_reason: resp.stop_reason,
            cost: Some(InferenceCost {
                input_cost_usd: 0.0,
                output_cost_usd: 0.0,
                total_cost_usd: 0.0,
            }),
            cached_tokens: 0,
        }
    }
}

#[async_trait]
impl LLMCore for MockLLMCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        self.infer_with_tools(context, &[]).await
    }

    async fn infer_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
    ) -> Result<InferenceResult, AgentOSError> {
        self.record_call(context, tools, MockCallMethod::InferWithTools);
        Ok(Self::make_result(self.pop_response()))
    }

    async fn infer_stream_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        self.record_call(context, tools, MockCallMethod::InferStreamWithTools);
        let resp = self.pop_response();

        // Emit text in 20-char chunks to simulate incremental streaming.
        // Chunking on char boundaries avoids splitting multi-byte UTF-8 characters.
        let chars: Vec<char> = resp.text.chars().collect();
        for chunk in chars.chunks(20) {
            let s: String = chunk.iter().collect();
            let _ = tx.send(InferenceEvent::Token(s)).await;
        }

        // Emit tool call events.
        for (i, call) in resp.tool_calls.iter().enumerate() {
            let _ = tx
                .send(InferenceEvent::ToolCallStart {
                    index: i,
                    id: call.id.clone(),
                    tool_name: call.tool_name.clone(),
                })
                .await;
            let _ = tx
                .send(InferenceEvent::ToolCallComplete(call.clone()))
                .await;
        }

        let result = Self::make_result(MockResponse {
            text: resp.text,
            tool_calls: resp.tool_calls,
            stop_reason: resp.stop_reason,
            tokens_used: resp.tokens_used,
        });
        let _ = tx.send(InferenceEvent::Done(result)).await;
        Ok(())
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        crate::types::HealthStatus::Healthy
    }

    fn provider_name(&self) -> &str {
        "mock"
    }

    fn model_name(&self) -> &str {
        "mock-model"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_mock_returns_responses_in_order() {
        let mock = MockLLMCore::new(vec![
            "First response".to_string(),
            "Second response".to_string(),
        ]);

        let ctx = ContextWindow::new(10);
        let r1 = mock.infer(&ctx).await.unwrap();
        assert_eq!(r1.text, "First response");
        assert_eq!(mock.call_count(), 1);

        let r2 = mock.infer(&ctx).await.unwrap();
        assert_eq!(r2.text, "Second response");
        assert_eq!(mock.call_count(), 2);

        // Exhausted
        let r3 = mock.infer(&ctx).await.unwrap();
        assert_eq!(r3.text, "No more mock responses");
    }

    #[tokio::test]
    async fn test_mock_health_check() {
        let mock = MockLLMCore::new(vec![]);
        assert!(mock.health_check().await.is_healthy());
    }

    #[tokio::test]
    async fn test_mock_returns_tool_calls() {
        let tool_call = InferenceToolCall {
            id: Some("call_123".to_string()),
            tool_name: "file-reader".to_string(),
            intent_type: "read".to_string(),
            payload: json!({"path": "/tmp/test.txt"}),
        };
        let mock = MockLLMCore::with_responses(vec![
            MockResponse::text("Thinking...").with_tool_calls(vec![tool_call.clone()])
        ]);

        let ctx = ContextWindow::new(10);
        let result = mock.infer_with_tools(&ctx, &[]).await.unwrap();

        assert_eq!(result.stop_reason, StopReason::ToolUse);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].tool_name, "file-reader");
        assert_eq!(result.tool_calls[0].id.as_deref(), Some("call_123"));
    }

    #[tokio::test]
    async fn test_mock_call_history_records_context_and_tools() {
        let mock = MockLLMCore::with_responses(vec![
            MockResponse::text("response 1"),
            MockResponse::text("response 2"),
        ]);

        let mut ctx = ContextWindow::new(100);
        use agentos_types::{ContextCategory, ContextEntry, ContextPartition};
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "hello".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::default(),
            is_summary: false,
        });

        mock.infer_with_tools(&ctx, &[]).await.unwrap();
        mock.infer_with_tools(&ctx, &[]).await.unwrap();

        let history = mock.call_history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].method, MockCallMethod::InferWithTools);
        assert_eq!(history[0].context_entries.len(), 1);
        assert_eq!(history[0].context_entries[0].0, ContextRole::User);
        assert_eq!(history[0].context_entries[0].1, "hello");
        assert_eq!(history[0].tool_names, Vec::<String>::new());
    }

    #[tokio::test]
    async fn test_mock_streaming_emits_events_in_order() {
        let tool_call = InferenceToolCall {
            id: Some("call_abc".to_string()),
            tool_name: "shell".to_string(),
            intent_type: "execute".to_string(),
            payload: json!({"command": "ls"}),
        };
        let mock = MockLLMCore::with_responses(vec![
            MockResponse::text("streaming text").with_tool_calls(vec![tool_call])
        ]);

        let ctx = ContextWindow::new(10);
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        mock.infer_stream_with_tools(&ctx, &[], tx).await.unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        // Verify ordering by position: Token < ToolCallStart < ToolCallComplete < Done.
        let first_token = events
            .iter()
            .position(|e| matches!(e, InferenceEvent::Token(_)))
            .expect("Should emit Token events");
        let first_tool_start = events
            .iter()
            .position(|e| matches!(e, InferenceEvent::ToolCallStart { .. }))
            .expect("Should emit ToolCallStart");
        let first_tool_complete = events
            .iter()
            .position(|e| matches!(e, InferenceEvent::ToolCallComplete(_)))
            .expect("Should emit ToolCallComplete");
        let done_pos = events
            .iter()
            .position(|e| matches!(e, InferenceEvent::Done(_)))
            .expect("Should emit Done");

        assert!(
            first_token < first_tool_start,
            "Token events should precede ToolCallStart"
        );
        assert!(
            first_tool_start < first_tool_complete,
            "ToolCallStart should precede ToolCallComplete"
        );
        assert!(
            first_tool_complete < done_pos,
            "ToolCallComplete should precede Done"
        );
        assert_eq!(done_pos, events.len() - 1, "Done should be the last event");
    }

    #[tokio::test]
    async fn test_mock_cost_is_zero() {
        let mock = MockLLMCore::new(vec!["hello".to_string()]);
        let ctx = ContextWindow::new(10);
        let result = mock.infer(&ctx).await.unwrap();
        let cost = result.cost.unwrap();
        assert_eq!(cost.total_cost_usd, 0.0);
    }
}
