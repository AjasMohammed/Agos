//! End-to-end bus flow: `RunTask` → `SpawnSubAgent` → child completes → `AwaitSubAgents`.
//!
//! Validates the full kernel command path used by CLI / tools for sub-agent coordination,
//! not only `TaskScheduler` helpers.

use crate::common;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::{TaskID, TaskState};
use serial_test::serial;
use std::time::Duration;

fn task_id_from_success(data: &serde_json::Value) -> TaskID {
    data["task_id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing task_id in {data}"))
        .parse()
        .expect("TaskID parse")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn bus_spawn_sub_agent_then_await_after_child_completes() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent(
        &kernel,
        "e2e-bus-parent",
        vec!["Parent answer.".to_string()],
    )
    .await;
    common::register_mock_agent(
        &kernel,
        "e2e-bus-child",
        vec!["Child answer.".to_string()],
    )
    .await;

    let run_resp = client
        .send_command(KernelCommand::RunTask {
            agent_name: Some("e2e-bus-parent".to_string()),
            prompt: "Say hello".to_string(),
            autonomous: false,
            no_checkpoint: true,
            thinking_level: Default::default(),
        })
        .await
        .expect("RunTask");

    let parent_data = match run_resp {
        KernelResponse::Success { data: Some(d) } => d,
        other => panic!("expected RunTask Success with data, got {other:?}"),
    };
    let parent_task_id = task_id_from_success(&parent_data);

    let spawn_resp = client
        .send_command(KernelCommand::SpawnSubAgent {
            parent_task_id,
            agent_name: "e2e-bus-child".to_string(),
            prompt: "Child work unit".to_string(),
            requested_permissions: vec![],
            context_slice: None,
        })
        .await
        .expect("SpawnSubAgent");

    let child_task_id = match spawn_resp {
        KernelResponse::SubAgentSpawned { child_task_id } => child_task_id,
        other => panic!("expected SubAgentSpawned, got {other:?}"),
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let state = kernel
            .scheduler
            .get_task(&child_task_id)
            .await
            .map(|t| t.state);
        if matches!(
            state,
            Some(TaskState::Complete | TaskState::Failed | TaskState::Cancelled)
        ) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timeout waiting for child terminal state, last={state:?}"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    let await_resp = client
        .send_command(KernelCommand::AwaitSubAgents {
            parent_task_id,
            child_task_ids: vec![child_task_id],
        })
        .await
        .expect("AwaitSubAgents");

    let results = match await_resp {
        KernelResponse::SubAgentResults { results } => results,
        other => panic!("expected SubAgentResults, got {other:?}"),
    };
    assert_eq!(results.len(), 1, "expected one child result");
    let summary = &results[0].1;
    assert!(
        summary.contains("complete"),
        "expected terminal success summary, got: {summary}"
    );

    kernel.shutdown();
    handle.await.expect("kernel run loop");
}
