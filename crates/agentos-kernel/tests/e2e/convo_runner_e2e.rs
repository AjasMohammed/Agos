//! End-to-end tests for the unified multi-agent conversation runner.
//!
//! The invariant under test is Phase 2's: **every turn writes exactly one
//! transcript row**, whatever happened. On 2026-09-09 a turn ran 20 tool
//! iterations and 12 operator approvals while the conversation stayed visibly
//! empty, because a turn that produced no text persisted nothing at all.

use crate::common;
use agentos_kernel::convo_runner::{run_convo, ConvoEvent};
use agentos_llm::{MockResponse, StopReason};
use serial_test::serial;
use std::sync::Arc;

/// Drain a runner event channel into a vec.
fn collect(
    mut rx: tokio::sync::mpsc::Receiver<ConvoEvent>,
) -> tokio::task::JoinHandle<Vec<ConvoEvent>> {
    tokio::spawn(async move {
        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            out.push(ev);
        }
        out
    })
}

/// A turn whose inference fails must still leave a row, and only then mark the
/// conversation `error`. Previously the failure path recorded nothing, so the
/// operator saw an empty conversation with no explanation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn failed_turn_persists_a_row_then_errors() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    // "ghost" is never registered, so inference fails on turn 1.
    let participants = vec!["ghost".to_string(), "also-ghost".to_string()];
    let convo_id = {
        let store = Arc::clone(&kernel.convo_store);
        store
            .create_convo("a topic", &participants, 4)
            .expect("create convo")
    };

    run_convo(&kernel, &convo_id, "a topic", &participants, 4, None).await;

    let store = Arc::clone(&kernel.convo_store);
    let turns = store.get_turns(&convo_id).expect("get turns");
    assert_eq!(
        turns.len(),
        1,
        "a failed turn must still leave exactly one transcript row"
    );
    assert!(
        turns[0].content.contains("turn failed"),
        "the row must say what happened; got: {}",
        turns[0].content
    );
    assert_eq!(store.get_convo(&convo_id).unwrap().unwrap().status, "error");

    kernel.shutdown();
    handle.await.unwrap();
}

/// A turn that runs but produces no text is `Silent`, not absent. The next
/// speaker sees the note, and so does the operator.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn silent_turn_persists_a_row_and_the_run_continues() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent_with_responses(
        &kernel,
        "quiet",
        vec![
            MockResponse::text("").with_stop_reason(StopReason::EndTurn),
            MockResponse::text("").with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;
    common::register_mock_agent_with_responses(
        &kernel,
        "talker",
        vec![MockResponse::text("I have plenty to say.").with_stop_reason(StopReason::EndTurn)],
    )
    .await;

    let participants = vec!["quiet".to_string(), "talker".to_string()];
    let convo_id = {
        let store = Arc::clone(&kernel.convo_store);
        store
            .create_convo("topic", &participants, 2)
            .expect("create")
    };

    run_convo(&kernel, &convo_id, "topic", &participants, 2, None).await;

    let store = Arc::clone(&kernel.convo_store);
    let turns = store.get_turns(&convo_id).expect("get turns");
    assert_eq!(turns.len(), 2, "both turns must be recorded");
    assert_eq!(turns[0].agent_name, "quiet");
    assert!(
        turns[0].content.contains("no reply"),
        "a silent turn must render as a visible note; got: {}",
        turns[0].content
    );
    assert_eq!(turns[1].agent_name, "talker");
    assert_eq!(
        store.get_convo(&convo_id).unwrap().unwrap().status,
        "complete",
        "a silent turn is not a failure — the run completes"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// A conversation stopped before the runner reaches a turn must still emit
/// `Done`. The browser's SSE client closes the stream on that event alone; with
/// no `Done` the page retries until it gives up, showing a live Stop button on a
/// conversation that ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn stopped_convo_emits_done_and_records_nothing() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent_with_responses(
        &kernel,
        "a",
        vec![MockResponse::text("hi").with_stop_reason(StopReason::EndTurn)],
    )
    .await;
    common::register_mock_agent_with_responses(
        &kernel,
        "b",
        vec![MockResponse::text("hello").with_stop_reason(StopReason::EndTurn)],
    )
    .await;

    let participants = vec!["a".to_string(), "b".to_string()];
    let convo_id = {
        let store = Arc::clone(&kernel.convo_store);
        let id = store
            .create_convo("topic", &participants, 4)
            .expect("create");
        store.set_status(&id, "stopped").expect("stop");
        id
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<ConvoEvent>(64);
    let drain = collect(rx);
    run_convo(&kernel, &convo_id, "topic", &participants, 4, Some(tx)).await;
    let events = drain.await.expect("drain");

    assert!(
        events.iter().any(|e| matches!(e, ConvoEvent::Done { .. })),
        "a stopped conversation must still emit Done; got {events:?}"
    );
    let store = Arc::clone(&kernel.convo_store);
    assert!(
        store.get_turns(&convo_id).expect("turns").is_empty(),
        "no turn should run once the convo is stopped"
    );
    assert_eq!(
        store.get_convo(&convo_id).unwrap().unwrap().status,
        "stopped",
        "the runner must not overwrite the operator's terminal status"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Round-robin order, and the streaming path emits a turn boundary per turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn round_robin_order_and_turn_events() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    for name in ["p1", "p2", "p3"] {
        common::register_mock_agent_with_responses(
            &kernel,
            name,
            vec![
                MockResponse::text(format!("{name} speaks")).with_stop_reason(StopReason::EndTurn),
                MockResponse::text(format!("{name} again")).with_stop_reason(StopReason::EndTurn),
            ],
        )
        .await;
    }

    let participants = vec!["p1".to_string(), "p2".to_string(), "p3".to_string()];
    let convo_id = {
        let store = Arc::clone(&kernel.convo_store);
        store
            .create_convo("topic", &participants, 4)
            .expect("create")
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<ConvoEvent>(256);
    let drain = collect(rx);
    run_convo(&kernel, &convo_id, "topic", &participants, 4, Some(tx)).await;
    let events = drain.await.expect("drain");

    let store = Arc::clone(&kernel.convo_store);
    let turns = store.get_turns(&convo_id).expect("turns");
    let order: Vec<String> = turns.iter().map(|t| t.agent_name.clone()).collect();
    assert_eq!(order, vec!["p1", "p2", "p3", "p1"], "round-robin order");

    let starts = events
        .iter()
        .filter(|e| matches!(e, ConvoEvent::TurnStart { .. }))
        .count();
    let ends = events
        .iter()
        .filter(|e| matches!(e, ConvoEvent::TurnEnd { .. }))
        .count();
    assert_eq!(starts, 4, "one TurnStart per turn");
    assert_eq!(ends, 4, "one TurnEnd per turn");

    kernel.shutdown();
    handle.await.unwrap();
}

/// Continue resumes the SAME conversation: a second run picks up after the last
/// speaker with the stored transcript, and an operator row posted in between is
/// answered without counting toward the turn budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn continue_resumes_after_last_speaker_with_operator_message() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent_with_responses(
        &kernel,
        "a",
        vec![
            MockResponse::text("a1").with_stop_reason(StopReason::EndTurn),
            MockResponse::text("a2").with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;
    common::register_mock_agent_with_responses(
        &kernel,
        "b",
        vec![
            MockResponse::text("b1").with_stop_reason(StopReason::EndTurn),
            MockResponse::text("b2").with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;

    let participants = vec!["a".to_string(), "b".to_string()];
    let store = Arc::clone(&kernel.convo_store);
    let convo_id = store
        .create_convo("topic", &participants, 3)
        .expect("create");

    // a, b, a — budget spent, next speaker is b.
    run_convo(&kernel, &convo_id, "topic", &participants, 3, None).await;
    assert_eq!(
        store.get_convo(&convo_id).unwrap().unwrap().status,
        "complete"
    );

    store
        .add_turn(
            &convo_id,
            agentos_kernel::convo_store::USER_SPEAKER,
            "wrap up",
            0,
        )
        .expect("operator row");
    let ceiling = store.claim_resume(&convo_id, 1).expect("claim");
    assert_eq!(ceiling, 4, "operator rows don't count toward the budget");

    run_convo(&kernel, &convo_id, "topic", &participants, ceiling, None).await;

    let turns = store.get_turns(&convo_id).expect("turns");
    let speakers: Vec<&str> = turns.iter().map(|t| t.agent_name.as_str()).collect();
    assert_eq!(speakers, ["a", "b", "a", "@user", "b"]);
    let numbers: Vec<u32> = turns.iter().map(|t| t.turn_number).collect();
    assert_eq!(numbers, [1, 2, 3, 4, 5]);
    assert_eq!(
        store.get_convo(&convo_id).unwrap().unwrap().status,
        "complete"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// An operator message waiting when the budget is spent earns one more round,
/// and the raised ceiling is persisted so the panel's "turn N of M" stays true.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn operator_message_past_budget_earns_a_round() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent_with_responses(
        &kernel,
        "a",
        vec![
            MockResponse::text("a1").with_stop_reason(StopReason::EndTurn),
            MockResponse::text("a2").with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;
    common::register_mock_agent_with_responses(
        &kernel,
        "b",
        vec![
            MockResponse::text("b1").with_stop_reason(StopReason::EndTurn),
            MockResponse::text("b2").with_stop_reason(StopReason::EndTurn),
        ],
    )
    .await;

    let participants = vec!["a".to_string(), "b".to_string()];
    let store = Arc::clone(&kernel.convo_store);
    let convo_id = store
        .create_convo("topic", &participants, 2)
        .expect("create");
    run_convo(&kernel, &convo_id, "topic", &participants, 2, None).await;

    store
        .add_turn(
            &convo_id,
            agentos_kernel::convo_store::USER_SPEAKER,
            "one more",
            0,
        )
        .expect("operator row");
    store.set_status(&convo_id, "running").expect("reopen");
    run_convo(&kernel, &convo_id, "topic", &participants, 2, None).await;

    let speakers: Vec<String> = store
        .get_turns(&convo_id)
        .expect("turns")
        .into_iter()
        .map(|t| t.agent_name)
        .collect();
    assert_eq!(speakers, ["a", "b", "@user", "a", "b"]);
    let convo = store.get_convo(&convo_id).unwrap().unwrap();
    assert_eq!((convo.status.as_str(), convo.max_turns), ("complete", 4));

    kernel.shutdown();
    handle.await.unwrap();
}
