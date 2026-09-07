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

/// CR1 "is it wired" guard: the chat path must fire the `ToolPre`/ApprovalHook
/// before executing a tool, exactly like the task-execution path. Under
/// `Deny` approval mode the hook aborts immediately (no escalation, no wait),
/// so a tool call in chat must surface as an approval-*blocked* result rather
/// than being executed. If the chat path ever stops firing ToolPre (the
/// original CR1 bug), this test fails because the tool would run / fail with a
/// non-approval error instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_tool_call_is_gated_by_approval_hook() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    // Deny mode: every non-readonly / unknown-risk tool is hard-rejected by
    // the ApprovalHook with an immediate Abort.
    {
        let resolver = kernel
            .approval_mode_resolver
            .as_ref()
            .expect("approval mode resolver must be wired at boot");
        let mut cfg = resolver.snapshot();
        cfg.mode = agentos_types::ApprovalMode::Deny;
        resolver.reload(cfg);
    }

    common::register_mock_agent_with_responses(
        &kernel,
        "chat-approval-agent",
        vec![
            // Unknown tool → ApprovalHook defaults it to ExecCapable
            // (fail-closed) → Deny mode aborts before execution.
            tool_call_response("shell-exec"),
            MockResponse::text("Understood, I will not run that.")
                .with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools(
            "chat-approval-agent",
            &[],
            "Run a shell command.",
            None,
            None,
        )
        .await
        .expect("chat_infer_with_tools failed");

    assert_eq!(result.tool_calls.len(), 1, "expected one tool call record");
    let call = &result.tool_calls[0];
    let err = call
        .result
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        err.contains("blocked") || err.contains("approval") || err.contains("denied"),
        "chat tool call must be gated by the approval hook (CR1); got result: {}",
        call.result
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Chat memory parity: a chat turn must leave the same episodic trail a task
/// leaves, and the agent's curated context memory must be injected back into
/// the chat prompt. Before this was wired, agents truthfully reported "I have
/// no record of our previous conversations" no matter how much they had
/// written to memory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_turn_writes_episodic_trail_and_injects_context_memory() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    common::register_mock_agent_with_responses(&kernel, "chat-memory-agent", vec![]).await;
    // Keep a typed handle so we can assert on what the model actually received,
    // not just on what the helper would have returned.
    let mock = std::sync::Arc::new(agentos_llm::MockLLMCore::with_responses(vec![
        tool_call_response("agent-manual"),
        MockResponse::text("Here is what the manual says.").with_stop_reason(StopReason::EndTurn),
    ]));

    let agent_id = {
        let registry = kernel.agent_registry.read().await;
        registry
            .get_by_name("chat-memory-agent")
            .expect("agent registered")
            .id
    };
    kernel.active_llms.write().await.insert(
        agent_id,
        std::sync::Arc::clone(&mock) as std::sync::Arc<dyn agentos_llm::LLMCore>,
    );
    kernel
        .context_memory_store
        .write(&agent_id.to_string(), "staging_db: 10.0.0.42", Some("test"))
        .await
        .expect("seed context memory");

    let result = kernel
        .chat_infer_with_tools(
            "chat-memory-agent",
            &[],
            "What tools do I have?",
            None,
            None,
        )
        .await
        .expect("chat_infer_with_tools failed");

    let timeline = kernel
        .episodic_memory
        .timeline_by_task(&result.task_id, 100)
        .await
        .expect("timeline");
    let kinds: Vec<String> = timeline
        .iter()
        .map(|e| e.entry_type.as_str().to_string())
        .collect();
    for expected in [
        "user_prompt",
        "tool_call",
        "tool_result",
        "llm_response",
        "system_event",
    ] {
        assert!(
            kinds.iter().any(|k| k == expected),
            "chat turn must record a `{expected}` episode; got {kinds:?}"
        );
    }
    let summary = timeline
        .iter()
        .find(|e| e.entry_type == agentos_memory::EpisodeType::SystemEvent)
        .expect("task summary episode");
    assert_eq!(
        summary
            .metadata
            .as_ref()
            .and_then(|m| m.get("outcome"))
            .and_then(|v| v.as_str()),
        Some("success"),
        "the summary row is what consolidation and background review key off"
    );

    // The seeded context memory actually reached the model's prompt — asserting
    // on `context_memory_block()` alone would still pass if the `ctx.push` were
    // dropped from the chat path.
    let first_call = mock.call_at(0).expect("mock was called");
    let system_text: String = first_call
        .context_entries
        .iter()
        .filter(|(role, _)| *role == agentos_types::ContextRole::System)
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        system_text.contains("<agent-context-memory>")
            && system_text.contains("staging_db: 10.0.0.42"),
        "context memory must be injected into the chat prompt"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// A turn that produced no answer must not be recorded as a success. The
/// background review and the consolidation engine both select on
/// `outcome: success`, and a turn that returned nothing is exactly the kind of
/// failure mode neither should learn a procedure from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn empty_answer_turn_is_recorded_as_degraded() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    common::register_mock_agent_with_responses(
        &kernel,
        "chat-degraded-agent",
        vec![MockResponse::text("").with_stop_reason(StopReason::EndTurn)],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools("chat-degraded-agent", &[], "Anything?", None, None)
        .await
        .expect("chat_infer_with_tools failed");

    let timeline = kernel
        .episodic_memory
        .timeline_by_task(&result.task_id, 100)
        .await
        .expect("timeline");
    let summary = timeline
        .iter()
        .find(|e| e.entry_type == agentos_memory::EpisodeType::SystemEvent)
        .expect("turn summary episode");
    assert_eq!(
        summary
            .metadata
            .as_ref()
            .and_then(|m| m.get("outcome"))
            .and_then(|v| v.as_str()),
        Some("degraded"),
        "an empty final answer is not a successful turn"
    );

    kernel.shutdown();
    handle.await.unwrap();
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
