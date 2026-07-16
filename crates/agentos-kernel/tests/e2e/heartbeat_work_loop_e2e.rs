//! End-to-end: the agent heartbeat drives the autonomous work loop.
//!
//! Boots a real kernel with the heartbeat enabled, seeds a work item for a mock
//! agent, and asserts the live run-loop tick wakes the agent, atomically claims
//! the item, runs it as a background task, and — via the completion linkage —
//! marks the item `Done`. This exercises the full Phase 3 wiring (selection →
//! checkout → enqueue → set_task → complete_by_task) on a running kernel, not in
//! isolation.

use crate::common;
use agentos_kernel::work_store::{NewWorkItem, WorkState};
use serial_test::serial;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn heartbeat_claims_and_completes_work_item() {
    // Enable the heartbeat at a 1s interval (no jitter). The run-loop tick that
    // hosts the heartbeat fires every ~10s, so the first wake lands ~10s in.
    let (kernel, _client, _tmp, handle) = common::setup_kernel_with(|cfg| {
        cfg.agent_heartbeat.default_interval_secs = 1;
        cfg.agent_heartbeat.jitter = 0.0;
        cfg.agent_heartbeat.max_wakes_per_tick = 8;
    })
    .await;

    // Mock agent whose single response is a final answer, so the spawned work
    // task completes successfully (→ complete_task_success → work item Done).
    common::register_mock_agent(&kernel, "hb-worker", vec!["Work complete.".to_string()]).await;

    let wq = kernel
        .work_queue
        .clone()
        .expect("work queue enabled at boot");
    let item_id = wq
        .create(NewWorkItem {
            assignee_agent: Some("hb-worker".to_string()),
            title: "verify-heartbeat".to_string(),
            prompt: "Perform the verification task and finish.".to_string(),
            priority: 5,
            ..Default::default()
        })
        .await
        .expect("seed work item");

    // Precondition: the item is claimable and nothing has touched it yet.
    assert_eq!(
        wq.state_of(&item_id).await.unwrap(),
        Some(WorkState::Pending),
        "freshly created item must start Pending"
    );

    // Poll up to ~60s for the live heartbeat to drive the item to a terminal
    // state. Record the last non-Pending state seen for a useful failure message.
    let mut last_seen = WorkState::Pending;
    let mut terminal = None;
    for _ in 0..120 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match wq.state_of(&item_id).await.unwrap() {
            Some(s) => {
                last_seen = s;
                if matches!(s, WorkState::Done | WorkState::Failed) {
                    terminal = Some(s);
                    break;
                }
            }
            None => panic!("work item vanished"),
        }
    }

    // The item must have left Pending (heartbeat claimed it)...
    assert_ne!(
        last_seen,
        WorkState::Pending,
        "heartbeat never claimed the work item (still Pending after 60s)"
    );
    // ...and completed successfully via the completion linkage.
    assert_eq!(
        terminal,
        Some(WorkState::Done),
        "heartbeat should have run the item to completion; last state was {last_seen:?}"
    );

    handle.abort();
}
