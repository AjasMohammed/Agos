//! End-to-end regression: provider-native `tool_call_id` survives the full
//! chat-inference round trip.
//!
//! Flow under test:
//!   1. First inference returns a native [`InferenceToolCall`] with
//!      `id = Some("call_native_abc123")`.
//!   2. Kernel pushes the assistant turn into context with
//!      `metadata.assistant_tool_calls` carrying the same id.
//!   3. Tool dispatch runs (tool fails — irrelevant, the error JSON still
//!      flows through `push_tool_result`).
//!   4. Kernel pushes the tool result with
//!      `metadata.tool_call_id = Some("call_native_abc123")`.
//!   5. Second inference is invoked with the updated context window.
//!
//! This test inspects the [`MockLLMCore`]'s recorded second call to assert
//! both metadata fields are present and correctly correlated. A regression
//! that silently drops the correlation id (e.g. an adapter or kernel change
//! that re-routes tool-result entries without propagating
//! `tool_call_id`) would be caught here.

use crate::common;
use agentos_kernel::Kernel;
use agentos_llm::{InferenceToolCall, MockLLMCore, MockResponse, StopReason};
use agentos_types::{
    AgentProfile, AgentStatus, ContextRole, LLMProvider, PermissionSet, ThinkingLevel,
};
use serial_test::serial;
use std::sync::Arc;

const NATIVE_TOOL_USE_ID: &str = "call_native_abc123";

async fn register_mock_agent_keep_handle(
    kernel: &Kernel,
    name: &str,
    responses: Vec<MockResponse>,
) -> Arc<MockLLMCore> {
    let agent_id = agentos_types::AgentID::new();
    let now = chrono::Utc::now();
    let profile = AgentProfile {
        id: agent_id,
        name: name.to_string(),
        provider: LLMProvider::Ollama,
        model: "mock-model".to_string(),
        status: AgentStatus::Online,
        permissions: PermissionSet::new(),
        roles: vec!["base".to_string()],
        current_task: None,
        description: String::new(),
        created_at: now,
        last_active: now,
        public_key_hex: None,
        base_url: None,
        default_thinking_level: ThinkingLevel::Off,
        system_prompt: None,
        manually_offline: false,
    };
    let mock = Arc::new(MockLLMCore::with_responses(responses));
    kernel.agent_registry.write().await.register(profile);
    kernel
        .active_llms
        .write()
        .await
        .insert(agent_id, mock.clone() as Arc<dyn agentos_llm::LLMCore>);
    mock
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn native_tool_call_id_round_trips_through_context() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    // Response 0: native tool call with a provider-supplied id.
    // The tool name is deliberately nonexistent so dispatch fails; the
    // resulting error JSON still flows through `push_tool_result` with
    // the same `tool_call_id`, which is what we want to verify.
    let resp_with_call =
        MockResponse::text("Calling tool.").with_tool_calls(vec![InferenceToolCall {
            id: Some(NATIVE_TOOL_USE_ID.to_string()),
            tool_name: "nonexistent-tool".to_string(),
            intent_type: "query".to_string(),
            payload: serde_json::json!({}),
        }]);
    // Response 1: terminal text, ends the loop.
    let resp_final = MockResponse::text("done").with_stop_reason(StopReason::EndTurn);

    let mock = register_mock_agent_keep_handle(
        &kernel,
        "native-tool-agent",
        vec![resp_with_call, resp_final],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools("native-tool-agent", &[], "use the tool please", None, None)
        .await
        .expect("chat_infer_with_tools failed");

    assert_eq!(
        result.iterations, 2,
        "expected exactly two inference iterations (call + final)"
    );
    assert_eq!(
        result.tool_calls.len(),
        1,
        "one tool call should be recorded"
    );

    let calls = mock.call_history();
    assert_eq!(
        calls.len(),
        2,
        "mock should have received exactly two inference calls"
    );

    // ── First inference: bare system + user, no assistant_tool_calls / tool_call_id yet
    let first = &calls[0];
    for entry in &first.active_snapshot {
        let meta = entry.metadata.as_ref();
        assert!(
            meta.and_then(|m| m.assistant_tool_calls.as_ref()).is_none(),
            "first inference must not see any assistant_tool_calls metadata yet"
        );
        assert!(
            meta.and_then(|m| m.tool_call_id.as_deref()).is_none(),
            "first inference must not see any tool_call_id metadata yet"
        );
    }

    // ── Second inference: must carry both metadata fields with matching ids.
    let second = &calls[1];

    // Find the Assistant entry that carries the tool-call metadata.
    let assistant_meta = second
        .active_snapshot
        .iter()
        .find(|e| e.role == ContextRole::Assistant)
        .and_then(|e| e.metadata.as_ref())
        .expect("second inference must include an Assistant entry with metadata");

    let assistant_tool_calls = assistant_meta
        .assistant_tool_calls
        .as_ref()
        .expect("assistant_tool_calls must be set on the assistant turn before tool dispatch");

    let calls_array = assistant_tool_calls
        .as_array()
        .expect("assistant_tool_calls must serialize to a JSON array");
    assert_eq!(
        calls_array.len(),
        1,
        "exactly one tool call should be recorded in metadata"
    );
    let recorded_id = calls_array[0]
        .get("id")
        .and_then(|v| v.as_str())
        .expect("recorded tool call must carry an `id` field");
    assert_eq!(
        recorded_id, NATIVE_TOOL_USE_ID,
        "assistant_tool_calls must preserve the provider-native id"
    );

    // Find the ToolResult entry pushed after dispatch.
    let tool_result_meta = second
        .active_snapshot
        .iter()
        .find(|e| e.role == ContextRole::ToolResult)
        .and_then(|e| e.metadata.as_ref())
        .expect("second inference must include a ToolResult entry with metadata");

    let result_id = tool_result_meta
        .tool_call_id
        .as_deref()
        .expect("ToolResult metadata must carry tool_call_id for native correlation");
    assert_eq!(
        result_id, NATIVE_TOOL_USE_ID,
        "ToolResult tool_call_id must match the provider-native id from the assistant turn"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Fallback regression: when an adapter does NOT emit a native id
/// (`InferenceToolCall.id == None`, e.g. Ollama / Gemini text-fallback),
/// neither `assistant_tool_calls[].id` nor `ToolResult.tool_call_id` is
/// set, but execution still completes. This protects against a future
/// refactor that synthesizes ids server-side and accidentally breaks
/// the "no id when the provider gave none" contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn fallback_path_has_no_tool_call_id_metadata() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    let resp_with_call =
        MockResponse::text("Calling tool.").with_tool_calls(vec![InferenceToolCall {
            id: None, // adapter did not provide a native id
            tool_name: "nonexistent-tool".to_string(),
            intent_type: "query".to_string(),
            payload: serde_json::json!({}),
        }]);
    let resp_final = MockResponse::text("done").with_stop_reason(StopReason::EndTurn);

    let mock = register_mock_agent_keep_handle(
        &kernel,
        "fallback-tool-agent",
        vec![resp_with_call, resp_final],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools("fallback-tool-agent", &[], "use a tool", None, None)
        .await
        .expect("chat_infer_with_tools failed");

    assert_eq!(result.iterations, 2);
    assert_eq!(result.tool_calls.len(), 1);

    let calls = mock.call_history();
    assert_eq!(calls.len(), 2);

    let second = &calls[1];
    let assistant_meta = second
        .active_snapshot
        .iter()
        .find(|e| e.role == ContextRole::Assistant)
        .and_then(|e| e.metadata.as_ref())
        .expect("assistant turn must still be recorded");
    let calls_array = assistant_meta
        .assistant_tool_calls
        .as_ref()
        .and_then(|v| v.as_array())
        .expect("assistant_tool_calls is still set even without a native id");
    assert_eq!(calls_array.len(), 1);
    assert!(
        calls_array[0].get("id").is_none_or(|v| v.is_null()),
        "id must be absent/null when the adapter did not provide one"
    );

    let tool_result_meta = second
        .active_snapshot
        .iter()
        .find(|e| e.role == ContextRole::ToolResult)
        .and_then(|e| e.metadata.as_ref())
        .expect("ToolResult must still be pushed");
    assert!(
        tool_result_meta.tool_call_id.is_none(),
        "ToolResult.tool_call_id must be None when no native id was supplied"
    );

    kernel.shutdown();
    handle.await.unwrap();
}
