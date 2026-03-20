use crate::common;
use serial_test::serial;

/// Helper: a minimal tool call block the LLM would emit.
fn tool_call_text(tool: &str) -> String {
    format!(
        "Let me look that up.\n```json\n{{\"tool\": \"{tool}\", \"intent_type\": \"query\", \"payload\": {{\"section\": \"tools\"}}}}\n```"
    )
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
        .chat_infer_with_tools("chat-test-agent", &[], "Hi there")
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
    common::register_mock_agent(
        &kernel,
        "chat-test-agent",
        vec![
            tool_call_text("nonexistent-tool"),
            "The tool is not available, but here is my answer anyway.".to_string(),
        ],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools("chat-test-agent", &[], "What tools are available?")
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

/// Loop stops at CHAT_MAX_TOOL_ITERATIONS (10) when the LLM keeps returning tool calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_chat_max_iterations() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    // Provide 10 tool call responses; the 10th triggers the iteration cap.
    let responses = vec![tool_call_text("loop-tool"); 10];
    common::register_mock_agent(&kernel, "chat-test-agent", responses).await;

    let result = kernel
        .chat_infer_with_tools("chat-test-agent", &[], "Loop forever please")
        .await
        .expect("chat_infer_with_tools failed");

    assert_eq!(result.iterations, 10, "must stop at exactly 10 iterations");
    assert!(
        result
            .answer
            .contains("[Note: Maximum tool call limit reached.]"),
        "expected warning in answer, got: {}",
        result.answer
    );
    // 9 tool calls are executed (iterations 1-9); iteration 10 hits the cap before tool exec.
    assert_eq!(
        result.tool_calls.len(),
        9,
        "expected 9 executed tool calls before cap"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// When a tool fails, the error JSON is injected into context and the LLM gets another turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_chat_tool_error_injected_and_llm_retries() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    common::register_mock_agent(
        &kernel,
        "chat-test-agent",
        vec![
            tool_call_text("broken-tool"),
            "I encountered an error but recovered with this answer.".to_string(),
        ],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools("chat-test-agent", &[], "Try a failing tool")
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
