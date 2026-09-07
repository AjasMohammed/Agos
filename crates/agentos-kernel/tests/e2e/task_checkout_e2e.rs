//! End-to-end: the atomic task checkout is claimed at dispatch and released on
//! terminal completion via the synchronous `RunTask` path.
//!
//! Regression guard for the bug where the release lived only in
//! `complete_task_success`/`failure` (the background/sub-agent paths) while the
//! synchronous `cmd_run_task` path — the one that actually claims — had its own
//! terminal arms that never released, leaking the claim until lease expiry.

use crate::common;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::TaskID;
use serial_test::serial;

fn task_id_from_success(data: &serde_json::Value) -> TaskID {
    data["task_id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing task_id in {data}"))
        .parse()
        .expect("TaskID parse")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn run_task_releases_checkout_on_completion() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent(&kernel, "e2e-checkout", vec!["Done.".to_string()]).await;

    let run_resp = client
        .send_command(KernelCommand::RunTask {
            agent_name: Some("e2e-checkout".to_string()),
            prompt: "Say hello".to_string(),
            autonomous: false,
            no_checkpoint: true,
            thinking_level: Default::default(),
        })
        .await
        .expect("RunTask");

    let data = match run_resp {
        KernelResponse::Success { data: Some(d) } => d,
        other => panic!("expected RunTask Success with data, got {other:?}"),
    };
    let task_id = task_id_from_success(&data);

    // The synchronous RunTask path returns only after the task is terminal, so by
    // here the checkout must already be released — no row should remain.
    let owner = kernel
        .task_checkout_store
        .owner_of(&task_id)
        .await
        .expect("owner_of");
    assert!(
        owner.is_none(),
        "checkout for completed task {task_id} should have been released, but is still owned by {owner:?}"
    );

    handle.abort();
}
