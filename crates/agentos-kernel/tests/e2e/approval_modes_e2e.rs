//! End-to-end tests for approval modes (Track B).
//!
//! These verify the resolver wiring + bus dispatch + persisted policy store
//! round-trip through a real booted kernel. The full ApprovalHook decision
//! path against a mock LLM live tool call is exercised inline by inspecting
//! `ApprovalMode::decide` against the resolver's view of an agent.

use crate::common;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::{ApprovalDecision, ApprovalMode, RiskClass};
use serial_test::serial;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn default_mode_is_ask_edit() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let resp = client
        .send_command(KernelCommand::GetApprovalConfig)
        .await
        .expect("GetApprovalConfig");
    match resp {
        KernelResponse::ApprovalConfigSnapshot {
            mode,
            agent_overrides,
        } => {
            assert_eq!(mode, "ask_edit");
            assert!(agent_overrides.is_empty());
        }
        other => panic!("expected ApprovalConfigSnapshot, got {other:?}"),
    }

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn set_mode_mutates_live_resolver() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    // Set global mode to "deny".
    client
        .send_command(KernelCommand::SetApprovalMode {
            mode: "deny".to_string(),
        })
        .await
        .expect("SetApprovalMode");

    // The live resolver must observe the new mode immediately.
    let resolver = kernel
        .approval_mode_resolver
        .as_ref()
        .expect("approval mode resolver wired at boot");
    let live = resolver.snapshot();
    assert_eq!(live.mode, ApprovalMode::Deny);

    // GetApprovalConfig round-trips the new value.
    let resp = client
        .send_command(KernelCommand::GetApprovalConfig)
        .await
        .expect("GetApprovalConfig after set");
    match resp {
        KernelResponse::ApprovalConfigSnapshot { mode, .. } => {
            assert_eq!(mode, "deny");
        }
        other => panic!("expected ApprovalConfigSnapshot, got {other:?}"),
    }

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn per_agent_override_takes_precedence_over_global() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;
    let _agent = common::register_mock_agent(&kernel, "scoped-bot", vec![]).await;

    // Global stays at the default (ask_edit). Override scoped-bot to auto.
    client
        .send_command(KernelCommand::SetApprovalAgentOverride {
            agent_name: "scoped-bot".into(),
            mode: "auto".into(),
        })
        .await
        .expect("SetApprovalAgentOverride");

    let resolver = kernel.approval_mode_resolver.as_ref().unwrap();
    let snap = resolver.snapshot();
    assert_eq!(snap.mode, ApprovalMode::AskEdit, "global unchanged");
    assert_eq!(
        snap.agent_overrides.get("scoped-bot").copied(),
        Some(ApprovalMode::Auto),
        "override wired",
    );

    // Clear it; the live resolver drops the entry.
    client
        .send_command(KernelCommand::ClearApprovalAgentOverride {
            agent_name: "scoped-bot".into(),
        })
        .await
        .expect("ClearApprovalAgentOverride");
    let snap = resolver.snapshot();
    assert!(snap.agent_overrides.is_empty(), "override cleared");

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn deny_mode_decides_writescoped_as_deny() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    client
        .send_command(KernelCommand::SetApprovalMode {
            mode: "deny".to_string(),
        })
        .await
        .expect("set deny");

    // The matrix is encoded in ApprovalMode::decide; verify it for the modes
    // an actual ApprovalHook would consult. Even under Deny, ReadonlyScoped
    // is still allowed (cheap, no escalation).
    assert_eq!(
        ApprovalMode::Deny.decide(RiskClass::ReadonlyScoped),
        ApprovalDecision::Allow
    );
    assert_eq!(
        ApprovalMode::Deny.decide(RiskClass::WriteScoped),
        ApprovalDecision::Deny
    );
    assert_eq!(
        ApprovalMode::Deny.decide(RiskClass::ExecCapable),
        ApprovalDecision::Deny
    );
    assert_eq!(
        ApprovalMode::Deny.decide(RiskClass::ControlPlane),
        ApprovalDecision::Deny
    );

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn control_plane_always_prompts_under_non_deny_modes() {
    // Mode-matrix invariant — ControlPlane never reaches Allow except via
    // the non-overridable floor. Track B pass-1 review flagged this as
    // critical to enforce in the hook; this test pins the matrix in place
    // so regressions surface immediately.
    for mode in [
        ApprovalMode::Auto,
        ApprovalMode::AskEdit,
        ApprovalMode::AskAlways,
    ] {
        assert_eq!(
            mode.decide(RiskClass::ControlPlane),
            ApprovalDecision::Prompt,
            "{mode:?} must always Prompt ControlPlane"
        );
    }
    assert_eq!(
        ApprovalMode::Deny.decide(RiskClass::ControlPlane),
        ApprovalDecision::Deny
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn add_list_revoke_approval_policy_round_trip() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    // List on fresh boot is empty.
    let resp = client
        .send_command(KernelCommand::ListApprovalPolicies)
        .await
        .expect("ListApprovalPolicies");
    match resp {
        KernelResponse::ApprovalPolicyList(list) => assert!(list.is_empty()),
        other => panic!("expected ApprovalPolicyList, got {other:?}"),
    }

    // Add a global allow-always for `file-writer` under a path glob.
    let resp = client
        .send_command(KernelCommand::AddApprovalPolicy {
            tool_name: "file-writer".into(),
            path_glob: Some("/tmp/agos-e2e/**".into()),
            agent_name: None,
        })
        .await
        .expect("AddApprovalPolicy");
    let policy_id = match resp {
        KernelResponse::ApprovalPolicyAdded {
            id,
            tool_name,
            path_glob,
            ..
        } => {
            assert_eq!(tool_name, "file-writer");
            assert_eq!(path_glob, Some("/tmp/agos-e2e/**".into()));
            id
        }
        other => panic!("expected ApprovalPolicyAdded, got {other:?}"),
    };

    // List now shows it.
    let resp = client
        .send_command(KernelCommand::ListApprovalPolicies)
        .await
        .expect("ListApprovalPolicies after add");
    match resp {
        KernelResponse::ApprovalPolicyList(list) => {
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].get("id").and_then(|v| v.as_i64()), Some(policy_id));
            assert_eq!(
                list[0].get("tool_name").and_then(|v| v.as_str()),
                Some("file-writer")
            );
        }
        other => panic!("expected ApprovalPolicyList, got {other:?}"),
    }

    // Revoke; list is empty again.
    let resp = client
        .send_command(KernelCommand::RevokeApprovalPolicy { id: policy_id })
        .await
        .expect("RevokeApprovalPolicy");
    match resp {
        KernelResponse::ApprovalPolicyRevoked { ok } => assert!(ok),
        other => panic!("expected ApprovalPolicyRevoked, got {other:?}"),
    }

    let resp = client
        .send_command(KernelCommand::ListApprovalPolicies)
        .await
        .expect("ListApprovalPolicies after revoke");
    match resp {
        KernelResponse::ApprovalPolicyList(list) => assert!(list.is_empty()),
        other => panic!("expected ApprovalPolicyList, got {other:?}"),
    }

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn invalid_mode_string_rejected() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let resp = client
        .send_command(KernelCommand::SetApprovalMode {
            mode: "bogus".to_string(),
        })
        .await
        .expect("SetApprovalMode bogus");
    match resp {
        KernelResponse::Error { message } => {
            assert!(
                message.to_lowercase().contains("unknown approval mode"),
                "expected parse error, got: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    kernel.shutdown();
    handle.await.unwrap();
}
