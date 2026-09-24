//! End-to-end regression for the 2026-09-21 collaboration deadlock.
//!
//! Convo `8a060bd4`: one agent scaffolded a project into its own home, the
//! other could not see it, and ten turns went into a permission grant that no
//! tool could issue. What has to hold now:
//!
//! 1. every participant gets the same shared directory, as a `ReadWrite` zone;
//! 2. a file one writes there, the other can read — with no `fs.workspace`
//!    permission, because a zone is kernel space;
//! 3. an agent's own home stays unreadable to its peer (the containment
//!    property this feature must not trade away);
//! 4. a refusal names the shared workspace and `workspace-request`;
//! 5. `ask-user` is reachable from a conversation turn, and nothing else on the
//!    withhold list is;
//! 6. a conversation that repeats itself ends `stalled`, not `complete`.

use crate::common;
use agentos_kernel::convo_runner::run_convo;
use agentos_kernel::kernel::convo_withholds;
use agentos_llm::{MockResponse, StopReason};
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentID, PermissionSet, StorageZoneQuery, TaskID, TraceID};
use serial_test::serial;
use std::sync::Arc;

/// A context that carries the zone query but NOT `fs.workspace` — the point is
/// that a shared-workspace path needs no host-folder permission.
fn zone_ctx(kernel: &agentos_kernel::Kernel, agent_id: AgentID) -> ToolExecutionContext {
    ToolExecutionContext {
        data_dir: kernel.data_dir().to_path_buf(),
        task_id: TaskID::new(),
        agent_id,
        trace_id: TraceID::new(),
        permissions: PermissionSet::new(),
        vault: None,
        hal: None,
        file_lock_registry: None,
        agent_registry: None,
        task_registry: None,
        escalation_query: None,
        workspace_paths: vec![],
        workspace_paths_writable: vec![],
        workspace_paths_executable: vec![],
        capability_registry: None,
        capability_dispatcher: None,
        storage_zone_query: Some(Arc::new(kernel.zone_table.clone()) as Arc<dyn StorageZoneQuery>),
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tool_categories: None,
        shared_dir: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn participants_share_a_workspace_and_can_hand_over_a_file() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    let a = common::register_mock_agent_with_responses(
        &kernel,
        "writer",
        vec![MockResponse::text("scaffolded it").with_stop_reason(StopReason::EndTurn)],
    )
    .await;
    let b = common::register_mock_agent_with_responses(
        &kernel,
        "runner",
        vec![MockResponse::text("on it").with_stop_reason(StopReason::EndTurn)],
    )
    .await;
    let outsider = common::register_mock_agent_with_responses(
        &kernel,
        "outsider",
        vec![MockResponse::text("not involved").with_stop_reason(StopReason::EndTurn)],
    )
    .await;

    let participants = vec!["writer".to_string(), "runner".to_string()];
    let convo_id = {
        let store = Arc::clone(&kernel.convo_store);
        store
            .create_convo("build a thing", &participants, 2)
            .expect("create convo")
    };

    run_convo(&kernel, &convo_id, "build a thing", &participants, 2, None).await;

    // 1. one zone each, same path, read-write.
    let zones_a = kernel.zone_table.list_for_agent(&a).await;
    let zones_b = kernel.zone_table.list_for_agent(&b).await;
    assert_eq!(
        zones_a.len(),
        1,
        "writer should hold exactly one convo zone"
    );
    assert_eq!(
        zones_b.len(),
        1,
        "runner should hold exactly one convo zone"
    );
    assert_eq!(
        zones_a[0].path, zones_b[0].path,
        "both participants must see the SAME directory — a per-agent path is the deadlock again"
    );
    assert!(
        kernel.zone_table.list_for_agent(&outsider).await.is_empty(),
        "a non-participant must not be given the conversation's workspace"
    );
    let shared = zones_a[0].path.clone();

    // 2. writer writes, runner reads — neither holds `fs.workspace`.
    let write = agentos_tools::file_writer::FileWriter
        .execute(
            serde_json::json!({
                "path": shared.join("init_db.py").to_string_lossy(),
                "content": "print('seeded')\n",
            }),
            zone_ctx(&kernel, a),
        )
        .await
        .expect("a participant may write into the shared workspace");
    assert_ne!(write.get("error"), Some(&serde_json::Value::Null));

    let read = agentos_tools::file_reader::FileReader
        .execute(
            serde_json::json!({ "path": shared.join("init_db.py").to_string_lossy() }),
            zone_ctx(&kernel, b),
        )
        .await
        .expect("the peer may read what was left in the shared workspace");
    assert!(
        read.get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("seeded"),
        "the peer must get the file contents back; got {read:?}"
    );

    // 3. the outsider is still locked out of the same path.
    let denied = agentos_tools::file_reader::FileReader
        .execute(
            serde_json::json!({ "path": shared.join("init_db.py").to_string_lossy() }),
            zone_ctx(&kernel, outsider),
        )
        .await;
    assert!(
        denied.is_err(),
        "an agent outside the conversation must not read its workspace"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// The containment property, stated as its own test so it cannot be quietly
/// traded away: agent homes stay private, and the refusal says where to put the
/// file instead. Every message in the original incident described the symptom
/// and none named an alternative, which is why the pair kept retrying.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn peer_home_stays_unreadable_and_the_refusal_names_the_remedy() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    let a = common::register_mock_agent(&kernel, "alpha", vec!["hi".into()]).await;
    let _b = common::register_mock_agent(&kernel, "beta", vec!["hi".into()]).await;

    let beta_home = kernel.data_dir().join("agents").join("beta");
    std::fs::create_dir_all(&beta_home).expect("create peer home");
    std::fs::write(beta_home.join("private.txt"), "secret").expect("seed peer home");

    let mut ctx = zone_ctx(&kernel, a);
    ctx.shared_dir = Some(kernel.data_dir().join("convos").join("c1").join("shared"));

    let err = agentos_tools::file_reader::FileReader
        .execute(
            serde_json::json!({ "path": beta_home.join("private.txt").to_string_lossy() }),
            ctx,
        )
        .await
        .expect_err("another agent's home is never readable");
    let msg = err.to_string();
    assert!(
        msg.contains("shared workspace"),
        "the refusal must name where the file SHOULD go; got: {msg}"
    );
    assert!(
        msg.contains("workspace-request"),
        "the refusal must name the tool that widens access; got: {msg}"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// `ask-user` left the withhold list on 2026-09-21; everything else on it
/// stayed. Both halves matter — the first was the missing exit from a deadlock,
/// the second is the containment rule the list exists for.
#[test]
fn convo_withholds_everything_but_the_operator_question() {
    assert!(
        !convo_withholds("ask-user"),
        "the operator must be reachable"
    );
    assert!(!convo_withholds("ask_user"), "underscore form too");
    for tool in [
        "agent-message",
        "channel-send",
        "notify-user",
        "spawn-agent",
        "task-spawn-async",
        "schedule-once",
        "set-timer",
        "a2a-delegate",
    ] {
        assert!(
            convo_withholds(tool),
            "{tool} must stay withheld in a convo"
        );
    }
}

/// Two agents repeating themselves is a deadlock, not a conversation. It used
/// to run to `max_turns` and close `complete`, which reads the same as a
/// finished conversation on the list.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn repeating_conversation_ends_stalled() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;

    let stuck = || {
        MockResponse::text("Please grant me access to that folder.")
            .with_stop_reason(StopReason::EndTurn)
    };
    common::register_mock_agent_with_responses(
        &kernel,
        "loop-a",
        vec![stuck(), stuck(), stuck(), stuck()],
    )
    .await;
    common::register_mock_agent_with_responses(
        &kernel,
        "loop-b",
        vec![stuck(), stuck(), stuck(), stuck()],
    )
    .await;

    let participants = vec!["loop-a".to_string(), "loop-b".to_string()];
    let convo_id = {
        let store = Arc::clone(&kernel.convo_store);
        store
            .create_convo("going nowhere", &participants, 8)
            .expect("create convo")
    };

    run_convo(&kernel, &convo_id, "going nowhere", &participants, 8, None).await;

    let store = Arc::clone(&kernel.convo_store);
    let convo = store.get_convo(&convo_id).unwrap().unwrap();
    assert_eq!(
        convo.status, "stalled",
        "a deadlocked conversation must be distinguishable from a finished one"
    );
    assert!(
        store.get_turns(&convo_id).expect("turns").len() < 8,
        "it must stop early, not spend the whole budget repeating itself"
    );
    let stalls = kernel
        .escalation_manager
        .list_pending()
        .await
        .into_iter()
        .filter(|e| e.metadata.get("kind").and_then(|v| v.as_str()) == Some("convo_stall"))
        .count();
    assert_eq!(stalls, 1, "exactly one stall escalation, not one per turn");

    kernel.shutdown();
    handle.await.unwrap();
}

/// One operator interruption per conversation turn, shared by `ask-user` and
/// `workspace-request`. Both park the turn on a human; a parked turn holds the
/// conversation, its status and the LLM slot, so the cost is occupancy as well
/// as volume. Claimed through the kernel so the claude-code gateway — which
/// never runs the chat loop — spends the same budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn one_operator_interruption_per_convo_turn() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent = common::register_mock_agent(&kernel, "asker", vec!["hi".into()]).await;

    // Outside a convo turn there is no budget at all.
    assert!(kernel.claim_operator_interruption(&agent).await);
    assert!(kernel.claim_operator_interruption(&agent).await);

    kernel.set_convo_turn(agent, Some(None)).await;
    assert!(
        kernel.claim_operator_interruption(&agent).await,
        "the first question of a turn goes through"
    );
    assert!(
        !kernel.claim_operator_interruption(&agent).await,
        "the second must be refused, whichever tool asks"
    );

    // The next turn starts with a fresh budget.
    kernel.set_convo_turn(agent, None).await;
    kernel.set_convo_turn(agent, Some(None)).await;
    assert!(kernel.claim_operator_interruption(&agent).await);

    kernel.shutdown();
    handle.await.unwrap();
}

/// An approval the system could not honour must not be reported as one —
/// not to the caller, not in the audit row, and not on the escalation list the
/// operator reads afterwards. Three approvals that changed nothing is what made
/// the original incident unrecoverable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn approval_whose_grant_fails_reports_denied() {
    use agentos_kernel::escalation::{AutoAction, ResolutionOutcome};
    use agentos_kernel::kernel_action::EscalationReason;

    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent = common::register_mock_agent(&kernel, "requester", vec!["hi".into()]).await;

    // `path` points inside the kernel data dir: the resolve-time validator must
    // refuse it even though the escalation says approve.
    let forbidden = kernel.data_dir().join("agents");
    let id = kernel
        .escalation_manager
        .create_escalation_with_metadata(
            TaskID::new(),
            agent,
            EscalationReason::AuthorizationRequired,
            "needs it".to_string(),
            "Grant access?".to_string(),
            vec!["approve".into(), "deny".into()],
            "high".to_string(),
            true,
            TraceID::new(),
            Some(AutoAction::Deny),
            serde_json::json!({
                "kind": "workspace_access",
                "path": forbidden.to_string_lossy(),
                "mode": "rwx",
            }),
        )
        .await;

    kernel.escalation_manager.prepare_resolution(id).await;
    let rx = kernel
        .escalation_manager
        .take_resolution_receiver(id)
        .await
        .expect("receiver installed");
    kernel
        .escalation_manager
        .resolve(id, "approve".to_string())
        .await
        .expect("resolves");

    assert_eq!(
        rx.await.expect("outcome delivered"),
        ResolutionOutcome::Denied,
        "a grant that could not be written must wake the caller as denied"
    );
    assert!(
        kernel
            .workspace_grants
            .list_for_agent(&agent)
            .into_iter()
            .all(|g| g.path != forbidden),
        "no grant may exist for a path the validator refused"
    );
    let resolution = kernel
        .escalation_manager
        .get(id)
        .await
        .and_then(|e| e.resolution)
        .unwrap_or_default();
    assert!(
        resolution.contains("grant failed"),
        "the operator must not see a bare 'approve' for a decision that was refused; got: {resolution}"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Widening the file resolver with storage zones must not widen the
/// `fs.workspace` gate: a real host-folder grant still requires that
/// permission. The two lists are deliberately different, and `file-move` is
/// where they are easiest to confuse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn host_grant_still_requires_the_workspace_permission() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent = common::register_mock_agent(&kernel, "grantee", vec!["hi".into()]).await;

    let dir = std::env::temp_dir().join(format!("agentos-ws-gate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create host dir");
    std::fs::write(dir.join("host.txt"), "host data").expect("seed");

    let mut ctx = zone_ctx(&kernel, agent);
    // A granted host folder, and NO `fs.workspace` in the permission set.
    ctx.workspace_paths = vec![dir.clone()];

    let err = agentos_tools::file_reader::FileReader
        .execute(
            serde_json::json!({ "path": dir.join("host.txt").to_string_lossy() }),
            ctx,
        )
        .await
        .expect_err("a host grant without fs.workspace must still be refused");
    assert!(
        err.to_string().contains("fs.workspace"),
        "the refusal must name the permission, not the path; got: {err}"
    );

    std::fs::remove_dir_all(&dir).ok();
    kernel.shutdown();
    handle.await.unwrap();
}
