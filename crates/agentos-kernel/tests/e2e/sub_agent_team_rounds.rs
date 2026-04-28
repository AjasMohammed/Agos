//! Verification tests for team coordinator await-round accounting (sub-agent coordination).
//!
//! These exercise `TaskScheduler::maybe_advance_team_await_round` through a live kernel
//! so fingerprint deduplication is validated on the same code path production uses.

use crate::common;
use agentos_types::AgentTask;
use serial_test::serial;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn kernel_scheduler_team_await_fingerprint_dedup() {
    let (kernel, _, _tmp, handle) = common::setup_kernel().await;

    let task = AgentTask {
        is_team_coordinator: true,
        team_max_rounds: Some(20),
        team_rounds_completed: 0,
        ..Default::default()
    };
    let tid = task.id;

    kernel.scheduler.register_external(task).await;

    let fp = "550e8400-e29b-41d4-a716-446655440001,550e8400-e29b-41d4-a716-446655440002";
    kernel
        .scheduler
        .maybe_advance_team_await_round(tid, fp)
        .await;
    kernel
        .scheduler
        .maybe_advance_team_await_round(tid, fp)
        .await;

    let t = kernel
        .scheduler
        .get_task(&tid)
        .await
        .expect("coordinator task");
    assert_eq!(
        t.team_rounds_completed, 1,
        "duplicate fingerprint must not double-count"
    );
    assert_eq!(t.team_last_await_round_fingerprint.as_deref(), Some(fp));

    kernel
        .scheduler
        .maybe_advance_team_await_round(tid, "660e8400-e29b-41d4-a716-446655440003")
        .await;
    let t2 = kernel
        .scheduler
        .get_task(&tid)
        .await
        .expect("coordinator task");
    assert_eq!(
        t2.team_rounds_completed, 2,
        "new child-set fingerprint advances round"
    );

    kernel.shutdown();
    handle.await.expect("kernel run loop");
}
