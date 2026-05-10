use crate::common;
use agentos_llm::{InferenceToolCall, MockResponse, StopReason};
use serial_test::serial;

/// Helper: a mock response that emits a native tool call.
fn tool_call_response(tool: &str) -> MockResponse {
    MockResponse::text("Let me look that up.").with_tool_calls(vec![InferenceToolCall {
        id: Some(format!("call_{tool}")),
        tool_name: tool.to_string(),
        intent_type: "query".to_string(),
        payload: serde_json::json!({"section": "tools"}),
    }])
}

/// Helper: same as `tool_call_response` but with a unique payload per call.
/// Used to bypass the dedup-streak circuit breaker when intentionally driving
/// the chat loop to its `max_tool_iterations` cap.
fn tool_call_response_with_payload(tool: &str, payload: serde_json::Value) -> MockResponse {
    MockResponse::text("Let me look that up.").with_tool_calls(vec![InferenceToolCall {
        id: Some(format!("call_{tool}")),
        tool_name: tool.to_string(),
        intent_type: "query".to_string(),
        payload,
    }])
}

/// Plain response with no tool call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_chat_no_tool_call() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    common::register_mock_agent(
        &kernel,
        "chat-test-agent",
        vec!["Hello! I can help with that.".to_string()],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools("chat-test-agent", &[], "Hi there", None, None)
        .await
        .expect("chat_infer_with_tools failed");

    assert_eq!(result.answer, "Hello! I can help with that.");
    assert_eq!(result.tool_calls.len(), 0, "no tool calls expected");
    assert_eq!(result.iterations, 1);

    kernel.shutdown();
    handle.await.unwrap();
}

/// LLM returns a tool call on the first inference; a plain answer on the second.
/// Tool execution fails (tool not found) but the error is injected back as context
/// and the LLM gets a second chance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_chat_tool_call_detected_and_executed() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    common::register_mock_agent_with_responses(
        &kernel,
        "chat-test-agent",
        vec![
            tool_call_response("nonexistent-tool"),
            MockResponse::text("The tool is not available, but here is my answer anyway.")
                .with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools(
            "chat-test-agent",
            &[],
            "What tools are available?",
            None,
            None,
        )
        .await
        .expect("chat_infer_with_tools failed");

    assert_eq!(
        result.answer,
        "The tool is not available, but here is my answer anyway."
    );
    assert_eq!(result.tool_calls.len(), 1, "expected one tool call record");
    assert_eq!(
        result.iterations, 2,
        "expected two LLM inference iterations"
    );

    let call = &result.tool_calls[0];
    assert_eq!(call.tool_name, "nonexistent-tool");
    // The tool failed — result should contain an error field.
    assert!(
        call.result.get("error").is_some(),
        "expected error in tool result, got: {}",
        call.result
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Loop stops at the configured `max_tool_iterations` cap when the LLM keeps
/// returning tool calls. Each iteration uses a unique tool name + payload so
/// the per-(tool, error) and dedup circuit breakers don't fire — this test
/// exercises the iteration cap itself, not the defensive guards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_chat_max_iterations() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    // 25 unique tool names + payloads cover the default cap (25) regardless
    // of how the kernel config is constructed by the test harness.
    let responses: Vec<MockResponse> = (0..25)
        .map(|i| {
            tool_call_response_with_payload(
                &format!("loop-tool-{i}"),
                serde_json::json!({"iter": i}),
            )
        })
        .collect();
    common::register_mock_agent_with_responses(&kernel, "chat-test-agent", responses).await;

    let result = kernel
        .chat_infer_with_tools("chat-test-agent", &[], "Loop forever please", None, None)
        .await
        .expect("chat_infer_with_tools failed");

    // Test config doesn't set `max_tool_iterations`, so the kernel falls back
    // to `CHAT_MAX_TOOL_ITERATIONS_FALLBACK` (25).
    let cap: u32 = if kernel.config.chat.max_tool_iterations == 0 {
        25
    } else {
        kernel.config.chat.max_tool_iterations
    };
    assert_eq!(
        result.iterations, cap,
        "must stop at exactly the configured cap"
    );
    assert!(
        result
            .answer
            .contains("[Note: Maximum tool call limit reached.]"),
        "expected warning in answer, got: {}",
        result.answer
    );
    // (cap - 1) tool calls are executed; the final iteration hits the cap before tool exec.
    assert_eq!(
        result.tool_calls.len() as u32,
        cap - 1,
        "expected (cap - 1) executed tool calls before cap"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// When a tool fails, the error JSON is injected into context and the LLM gets another turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_chat_tool_error_injected_and_llm_retries() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    common::register_mock_agent_with_responses(
        &kernel,
        "chat-test-agent",
        vec![
            tool_call_response("broken-tool"),
            MockResponse::text("I encountered an error but recovered with this answer.")
                .with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools("chat-test-agent", &[], "Try a failing tool", None, None)
        .await
        .expect("chat_infer_with_tools failed");

    assert_eq!(result.iterations, 2, "LLM should be called twice");
    assert_eq!(result.tool_calls.len(), 1);

    let call = &result.tool_calls[0];
    assert_eq!(call.tool_name, "broken-tool");
    assert!(
        call.result.get("error").is_some(),
        "error must be recorded in tool call record"
    );

    assert_eq!(
        result.answer,
        "I encountered an error but recovered with this answer."
    );

    kernel.shutdown();
    handle.await.unwrap();
}
