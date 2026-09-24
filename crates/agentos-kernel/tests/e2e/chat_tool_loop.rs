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

/// The turn-scope gate is the actual enforcement point for a conversation turn,
/// not the manifest filter — a model routinely emits a tool name that was never
/// offered to it, and on 2026-09-09 one did exactly that: a convo turn called
/// `agent-message` twelve times, each one raising a human approval prompt and
/// writing a DM the group chat could not render, while the transcript stayed
/// empty. Withholding the schema alone would not have stopped any of it.
///
/// Approval mode is left at its default here on purpose: the scope check must
/// reject *ahead* of the capability and approval gates, so the operator is never
/// prompted for a call a convo turn was not allowed to make in the first place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn convo_scope_rejects_withheld_tool_at_dispatch() {
    use agentos_kernel::kernel::ChatTurnScope;

    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent_with_responses(
        &kernel,
        "convo-scope-agent",
        vec![
            tool_call_response("agent-message"),
            MockResponse::text("Fine, I will just say it out loud.")
                .with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools_scoped(
            "convo-scope-agent",
            &[],
            "Open the conversation.",
            None,
            None,
            ChatTurnScope::ConvoTurn { shared_dir: None },
        )
        .await
        .expect("chat_infer_with_tools_scoped failed");

    assert_eq!(result.tool_calls.len(), 1, "expected one tool call record");
    let call = &result.tool_calls[0];
    let err = call
        .result
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        err.contains("not available inside an agent conversation"),
        "a convo turn must be refused the out-of-band egress family; got result: {}",
        call.result
    );
    // The refusal has to tell the model what to do instead, or it spends the
    // remaining iterations retrying the same call.
    assert!(
        err.contains("plain text"),
        "the refusal must point the model at replying instead; got: {err}"
    );
    // The loop keeps going and the turn still produces speech.
    assert!(result.answer.contains("say it out loud"));

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
        // Two blanks: the loop nudges once on an empty final answer before
        // giving up, so a single blank would be masked by the retry.
        vec![
            MockResponse::text("").with_stop_reason(StopReason::EndTurn),
            MockResponse::text("").with_stop_reason(StopReason::EndTurn),
        ],
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

/// A blank final answer (EndTurn, no text, no tool calls — seen on gpt-oss
/// and nemotron after a tool-result burst) gets exactly one nudge retry. The
/// second inference must see the nudge as the last user entry, and its text
/// is what the user receives — not the placeholder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn empty_answer_is_retried_once_with_nudge() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id =
        common::register_mock_agent_with_responses(&kernel, "chat-nudge-agent", vec![]).await;
    // Swap in a mock we keep a handle to, so the retry call's context can be
    // inspected after the turn.
    let mock = std::sync::Arc::new(agentos_llm::MockLLMCore::with_responses(vec![
        MockResponse::text("").with_stop_reason(StopReason::EndTurn),
        MockResponse::text("Here is the answer.").with_stop_reason(StopReason::EndTurn),
    ]));
    kernel.active_llms.write().await.insert(
        agent_id,
        mock.clone() as std::sync::Arc<dyn agentos_llm::LLMCore>,
    );

    let result = kernel
        .chat_infer_with_tools("chat-nudge-agent", &[], "Anything?", None, None)
        .await
        .expect("chat_infer_with_tools failed");

    assert_eq!(result.answer, "Here is the answer.");
    assert_eq!(result.iterations, 2, "one blank + one retry");

    let history = mock.call_history();
    assert_eq!(history.len(), 2);
    let (role, text) = history[1]
        .context_entries
        .last()
        .expect("retry call has context");
    assert_eq!(*role, agentos_types::ContextRole::User);
    assert!(
        text.contains("previous reply was empty"),
        "retry must carry the nudge, got: {text}"
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

    // Both pieces the model spoke, not just the closing one: the text that
    // came with the tool call is part of the turn's answer (2026-09-20 — an
    // agent that narrated each step showed only its last line on refetch).
    assert_eq!(
        result.answer,
        "Let me look that up.\n\nThe tool is not available, but here is my answer anyway."
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
        "Let me look that up.\n\nI encountered an error but recovered with this answer."
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Withholding a tool from the offered manifest list is not enforcement: models
/// routinely emit names that were never offered. A convo turn that calls one
/// anyway must be refused at dispatch, and told what to do instead.
///
/// Regression guard for 2026-09-09, when a convo turn spent 20 iterations and 12
/// operator approvals sending `agent-message` DMs while the conversation the
/// operator was watching stayed empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn convo_turn_refuses_withheld_tool_at_dispatch() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent_with_responses(
        &kernel,
        "convo-scope-agent",
        vec![
            tool_call_response("agent-message"),
            MockResponse::text("Understood — here is my reply instead.")
                .with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools_scoped(
            "convo-scope-agent",
            &[],
            "Say hello to the other participant.",
            None,
            None,
            agentos_kernel::kernel::ChatTurnScope::ConvoTurn { shared_dir: None },
        )
        .await
        .expect("chat_infer_with_tools_scoped failed");

    assert_eq!(result.tool_calls.len(), 1, "expected one tool call record");
    let err = result.tool_calls[0]
        .result
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        err.contains("not available inside an agent conversation"),
        "withheld tool must be refused with the scope message; got: {err}"
    );
    assert!(
        err.contains("Reply with plain text"),
        "the refusal must tell the model what to do instead, or it retries; got: {err}"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// The same call is allowed on an ordinary chat turn — proves the refusal above
/// comes from the turn scope, not from a permission or approval denial.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn full_scope_does_not_refuse_messaging_tools() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent_with_responses(
        &kernel,
        "full-scope-agent",
        vec![
            tool_call_response("agent-message"),
            MockResponse::text("Sent.").with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;

    let result = kernel
        .chat_infer_with_tools(
            "full-scope-agent",
            &[],
            "Message the other agent.",
            None,
            None,
        )
        .await
        .expect("chat_infer_with_tools failed");

    let err = result.tool_calls[0]
        .result
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        !err.contains("not available inside an agent conversation"),
        "an ordinary chat turn must not hit the convo scope gate; got: {err}"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// A convo turn is capped well below the general chat cap. The turn that caused
/// this work ran 20 iterations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn convo_turn_caps_tool_iterations() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    // Far more tool calls than the convo cap allows, each with a distinct
    // payload so the dedup circuit breaker doesn't end the loop first, and each
    // one SUCCEEDING so the identical-failure breaker (3 in a convo turn) does
    // not end it either — the cap is what is under test here.
    let responses: Vec<MockResponse> = (0..20)
        .map(|i| {
            tool_call_response_with_payload(
                "think",
                serde_json::json!({"thought": format!("step {i}")}),
            )
        })
        .collect();
    let agent_id =
        common::register_mock_agent_with_responses(&kernel, "convo-cap-agent", vec![]).await;
    let mock = std::sync::Arc::new(agentos_llm::MockLLMCore::with_responses(responses));
    kernel.active_llms.write().await.insert(
        agent_id,
        mock.clone() as std::sync::Arc<dyn agentos_llm::LLMCore>,
    );

    let result = kernel
        .chat_infer_with_tools_scoped(
            "convo-cap-agent",
            &[],
            "Keep going.",
            None,
            None,
            agentos_kernel::kernel::ChatTurnScope::ConvoTurn { shared_dir: None },
        )
        .await
        .expect("chat_infer_with_tools_scoped failed");

    let cap = agentos_kernel::kernel::CONVO_TURN_MAX_TOOL_ITERATIONS as usize;
    assert_eq!(
        result.iterations as usize, cap,
        "a convo turn must stop at the cap"
    );
    assert_eq!(
        result.tool_calls.len(),
        cap - 1,
        "the capped call is dropped"
    );

    // Only the last inference is told its tool calls won't run.
    let history = mock.call_history();
    assert_eq!(history.len(), cap);
    let told = |i: usize| {
        history[i]
            .context_entries
            .iter()
            .any(|(_, t)| t.contains("last step this turn"))
    };
    assert!(told(cap - 1), "final iteration must carry the nudge");
    assert!(!told(cap - 2), "earlier iterations must not");

    kernel.shutdown();
    handle.await.unwrap();
}

/// Pressing Stop in the browser aborts the fetch, which drops the SSE stream
/// and with it the receiver on the kernel's event channel. The turn must end
/// with whatever text already reached the reader — returning `Err` made both
/// callers persist nothing, so the half-written reply vanished from the
/// transcript on the next refetch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn stopped_stream_keeps_the_partial_answer() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    // Longer than the 64-slot channel holds at the mock's 20 chars per chunk,
    // so the kernel is still sending when the reader goes away.
    let long_answer = "The first twenty chars and a lot more after them. ".repeat(40);
    common::register_mock_agent_with_responses(
        &kernel,
        "chat-stop-agent",
        vec![MockResponse::text(&long_answer).with_stop_reason(StopReason::EndTurn)],
    )
    .await;

    // Read until the answer starts, then drop the receiver: what an aborted
    // fetch leaves behind mid-reply.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<agentos_kernel::ChatStreamEvent>(64);
    let reader = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if matches!(ev, agentos_kernel::ChatStreamEvent::TextChunk { .. }) {
                break;
            }
        }
        drop(rx);
    });

    let result = kernel
        .chat_infer_streaming(
            "chat-stop-agent",
            &[],
            "Say something long.",
            None,
            tx,
            None,
        )
        .await
        .expect("a stopped stream must still return the partial turn");
    reader.await.unwrap();

    assert!(
        result.answer.contains("The first twenty"),
        "text already streamed must survive the stop; got {:?}",
        result.answer
    );
    assert!(
        result.answer.contains("[Note: stopped"),
        "the transcript must say the reply was cut short; got {:?}",
        result.answer
    );
    assert!(
        result.answer.len() < long_answer.len(),
        "a stopped turn must not carry the whole answer; got {} of {} chars",
        result.answer.len(),
        long_answer.len()
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Stop pressed before the first token (or a reader that never read at all):
/// the turn still closes cleanly instead of failing, and it never pays for an
/// inference — the check runs on the first send of the iteration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn stream_with_no_reader_ends_the_turn_without_failing() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent_with_responses(
        &kernel,
        "chat-noreader-agent",
        vec![MockResponse::text("Never read by anyone.").with_stop_reason(StopReason::EndTurn)],
    )
    .await;

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    drop(rx);

    let result = kernel
        .chat_infer_streaming("chat-noreader-agent", &[], "Anyone there?", None, tx, None)
        .await
        .expect("a reader that is already gone is not a turn failure");

    assert!(
        result
            .answer
            .contains("[Note: stopped before the reply started.]"),
        "got {:?}",
        result.answer
    );
    assert_eq!(
        result.tokens_used, 0,
        "the turn must bail before paying for an inference"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// The `task_id` on `ToolStart` must be the id the escalation is filed under.
///
/// A client correlates the inline approval card to the live turn by that id and
/// by nothing else, so the moment the stream and the approval gate disagree the
/// card is unmatchable and approvals silently stop appearing in chat — which is
/// exactly what happened when the gate moved onto the per-turn id while the
/// stream still carried a per-iteration one (2026-09-18).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn tool_start_task_id_matches_the_escalation_it_raises() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    {
        let resolver = kernel
            .approval_mode_resolver
            .as_ref()
            .expect("approval mode resolver must be wired at boot");
        let mut cfg = resolver.snapshot();
        cfg.mode = agentos_types::ApprovalMode::AskAlways;
        resolver.reload(cfg);
    }

    common::register_mock_agent_with_permissions(
        &kernel,
        "chat-card-agent",
        vec![
            // A payload that actually validates: schema validation runs ahead of
            // the approval gate, so a malformed call never reaches it.
            tool_call_response_with_payload(
                "shell-exec",
                serde_json::json!({ "command": "echo hi" }),
            ),
            MockResponse::text("Understood.").with_stop_reason(StopReason::EndTurn),
        ],
        // The capability check runs ahead of the approval gate, so the agent has
        // to be allowed to execute before approval is the thing standing in the
        // way.
        agentos_types::PermissionSet {
            entries: vec![agentos_types::PermissionEntry {
                resource: "process.exec".to_string(),
                read: true,
                write: true,
                execute: true,
                query: true,
                observe: true,
                expires_at: None,
            }],
            deny_entries: vec![],
        },
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<agentos_kernel::ChatStreamEvent>(64);
    let escalations = std::sync::Arc::clone(&kernel.escalation_manager);
    // Reads the stream like the panel does: take the id off `ToolStart`, look
    // for the escalation under it, then answer so the parked turn can finish.
    let reader = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let agentos_kernel::ChatStreamEvent::ToolStart { task_id, .. } = ev else {
                continue;
            };
            let streamed: agentos_types::TaskID = task_id
                .expect("a chat tool call must stream the task id its approval is filed under")
                .parse()
                .expect("task_id must be a well-formed id");
            // The gate raises the escalation just after the frame is sent.
            for _ in 0..50 {
                let found = escalations.for_task(&streamed).await;
                if let Some(esc) = found.into_iter().find(|e| !e.resolved) {
                    escalations.resolve(esc.id, "deny".to_string()).await;
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            return false;
        }
        false
    });

    let result = kernel
        .chat_infer_streaming(
            "chat-card-agent",
            &[],
            "Run a shell command.",
            None,
            tx,
            None,
        )
        .await
        .expect("chat_infer_streaming failed");

    assert!(
        reader.await.unwrap(),
        "no pending escalation was filed under the task id `ToolStart` streamed — \
         an inline approval card could never be matched to this turn"
    );
    assert_eq!(result.tool_calls.len(), 1, "expected one tool call record");

    kernel.shutdown();
    handle.await.unwrap();
}

/// Three identical failures end a convo turn, with the reason in the text.
///
/// The dedup cache replays identical *successes*; a call that fails the same
/// way every time is never deduped, so before this a doomed probe spent the
/// whole iteration budget. On 2026-09-21 four convo turns died that way,
/// re-globbing a path that could not resolve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn convo_turn_stops_after_three_identical_failures() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    // Same tool, same failure, distinct payloads so dedup never fires.
    let responses: Vec<MockResponse> = (0..8)
        .map(|i| {
            tool_call_response_with_payload(
                "file-reader",
                serde_json::json!({"path": format!("/definitely/not/here/{i}.txt")}),
            )
        })
        .collect();
    let agent_id =
        common::register_mock_agent_with_responses(&kernel, "convo-fail-agent", vec![]).await;
    let mock = std::sync::Arc::new(agentos_llm::MockLLMCore::with_responses(responses));
    kernel.active_llms.write().await.insert(
        agent_id,
        mock.clone() as std::sync::Arc<dyn agentos_llm::LLMCore>,
    );

    let result = kernel
        .chat_infer_with_tools_scoped(
            "convo-fail-agent",
            &[],
            "Find it.",
            None,
            None,
            agentos_kernel::kernel::ChatTurnScope::ConvoTurn { shared_dir: None },
        )
        .await
        .expect("chat_infer_with_tools_scoped failed");

    assert!(
        result.iterations < agentos_kernel::kernel::CONVO_TURN_MAX_TOOL_ITERATIONS,
        "the turn must end before the iteration cap; ran {} iterations",
        result.iterations
    );
    assert!(
        result.answer.contains("kept failing with the same error"),
        "the transcript must carry the real reason; got: {}",
        result.answer
    );

    kernel.shutdown();
    handle.await.unwrap();
}
