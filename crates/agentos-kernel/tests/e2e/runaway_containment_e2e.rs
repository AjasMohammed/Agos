//! End-to-end: the containment layers added after the 2026-07-26 trigger-loop
//! incident, where agent `nimo` self-triggered ~88k tasks/hour and left 190k
//! queued tasks that replayed on every kernel restart.
//!
//! Covers the operator-facing halves that unit tests can't reach:
//! - `KernelCommand::PurgeTasks` drains an agent's backlog live, in memory and
//!   in persistence, without stopping the kernel.
//! - A paused agent's queued work stays queued instead of being drained by the
//!   executor.
//!
//! The failure-streak breaker's counting logic is unit-tested in
//! `task_completion.rs` (`apply_failure_streak`) rather than here — `MockLLMCore`
//! has no error path, so an e2e cannot drive real fast failures.

use crate::common;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::{AgentID, AgentTask, CapabilityToken, PermissionSet, TaskID, TaskState};
use serial_test::serial;
use std::collections::BTreeSet;
use std::time::Duration;

fn queued_task(agent_id: AgentID, prompt: &str) -> AgentTask {
    let mut task = AgentTask::default();
    task.id = TaskID::new();
    task.agent_id = agent_id;
    task.state = TaskState::Queued;
    task.priority = 5;
    task.original_prompt = prompt.to_string();
    task.timeout = Duration::from_secs(300);
    task.capability_token = CapabilityToken {
        task_id: task.id,
        agent_id,
        allowed_tools: BTreeSet::new(),
        allowed_intents: BTreeSet::new(),
        permissions: PermissionSet::new(),
        issued_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        signature: Vec::new(),
    };
    task
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn purge_tasks_drains_backlog_without_restart() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let agent_id = common::register_mock_agent(&kernel, "e2e-purge", vec!["ok".to_string()]).await;

    // Pause the agent first so the executor cannot drain the queue underneath
    // the assertions — this is also the documented recovery order.
    kernel
        .agent_registry
        .write()
        .await
        .set_offline(&agent_id, true);

    for i in 0..25 {
        kernel
            .scheduler
            .enqueue(queued_task(agent_id, &format!("backlog {i}")))
            .await;
    }
    assert_eq!(
        kernel.scheduler.queued_count_for_agent(&agent_id).await,
        25,
        "backlog should be queued before the purge"
    );

    let resp = client
        .send_command(KernelCommand::PurgeTasks {
            agent_id,
            states: vec!["queued".to_string()],
        })
        .await
        .expect("PurgeTasks");

    match resp {
        KernelResponse::Success { data: Some(d) } => {
            assert_eq!(d["purged"].as_u64(), Some(25), "all queued tasks purged");
        }
        other => panic!("expected PurgeTasks Success with data, got {other:?}"),
    }

    assert_eq!(
        kernel.scheduler.queued_count_for_agent(&agent_id).await,
        0,
        "queue must be empty after the purge — this is the recovery path that \
         previously required stopping the kernel and running raw SQL"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn purge_rejects_unknown_state() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "e2e-purge-bad", vec!["ok".into()]).await;

    let resp = client
        .send_command(KernelCommand::PurgeTasks {
            agent_id,
            states: vec!["running".to_string()],
        })
        .await
        .expect("PurgeTasks");

    match resp {
        KernelResponse::Error { message } => {
            assert!(
                message.contains("running is never purged"),
                "error should steer the operator to `task cancel`, got: {message}"
            );
        }
        other => panic!("expected an Error for state=running, got {other:?}"),
    }

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn paused_agent_backlog_is_held_not_executed() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    let paused_id =
        common::register_mock_agent(&kernel, "e2e-paused", vec!["ok".to_string()]).await;

    kernel
        .agent_registry
        .write()
        .await
        .set_offline(&paused_id, true);

    for i in 0..5 {
        kernel
            .scheduler
            .enqueue(queued_task(paused_id, &format!("held {i}")))
            .await;
    }

    // Give the executor loop (100ms tick) several chances to pick work up.
    tokio::time::sleep(Duration::from_millis(600)).await;

    assert_eq!(
        kernel.scheduler.queued_count_for_agent(&paused_id).await,
        5,
        "a paused agent's queued tasks must stay queued — before this guard, \
         `manually_offline` only stopped boot reactivation while the executor \
         happily drained the backlog"
    );

    handle.abort();
}
