//! End-to-end tests for user filesystem grants (Track A).
//!
//! Exercise the full `agentos workspace grant/revoke/list` → bus → kernel →
//! WorkspaceGrantRegistry → ToolExecutionContext path through a real booted
//! kernel. These would have caught the parallel-batch bug surfaced in the
//! Track A pass-1 review where `self.workspace_paths.clone()` bypassed the
//! per-agent registry.

use crate::common;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::{WorkspaceGrant, WorkspaceGrantMode};
use serial_test::serial;
use std::path::PathBuf;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn grant_revoke_list_round_trip() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let path = PathBuf::from("/tmp/agos-e2e-grant-test");
    // Initial list is empty (no legacy config paths in the test config).
    let resp = client
        .send_command(KernelCommand::ListWorkspaceGrants { agent_name: None })
        .await
        .expect("ListWorkspaceGrants");
    match resp {
        KernelResponse::WorkspaceGrantList(g) => {
            assert!(g.is_empty(), "expected empty grant list at boot");
        }
        other => panic!("expected WorkspaceGrantList, got {other:?}"),
    }

    // Grant.
    let resp = client
        .send_command(KernelCommand::GrantWorkspace {
            path: path.clone(),
            agent_name: None,
            mode: "rwx".to_string(),
        })
        .await
        .expect("GrantWorkspace");
    let granted: WorkspaceGrant = match resp {
        KernelResponse::WorkspaceGrantCreated(g) => g,
        other => panic!("expected WorkspaceGrantCreated, got {other:?}"),
    };
    assert_eq!(granted.path, path);
    assert!(granted.agent_id.is_none(), "expected global grant");
    assert_eq!(granted.mode, WorkspaceGrantMode::READ_WRITE_EXEC);

    // List shows it.
    let resp = client
        .send_command(KernelCommand::ListWorkspaceGrants { agent_name: None })
        .await
        .expect("ListWorkspaceGrants");
    match resp {
        KernelResponse::WorkspaceGrantList(list) => {
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].path, path);
        }
        other => panic!("expected WorkspaceGrantList, got {other:?}"),
    }

    // Revoke.
    let resp = client
        .send_command(KernelCommand::RevokeWorkspace {
            path: path.clone(),
            agent_name: None,
        })
        .await
        .expect("RevokeWorkspace");
    match resp {
        KernelResponse::WorkspaceGrantRevoked { count } => {
            assert_eq!(count, 1, "expected one row revoked");
        }
        other => panic!("expected WorkspaceGrantRevoked, got {other:?}"),
    }

    // Revoke again is a no-op (count = 0).
    let resp = client
        .send_command(KernelCommand::RevokeWorkspace {
            path: path.clone(),
            agent_name: None,
        })
        .await
        .expect("RevokeWorkspace second call");
    match resp {
        KernelResponse::WorkspaceGrantRevoked { count } => {
            assert_eq!(count, 0, "expected no rows for second revoke");
        }
        other => panic!("expected WorkspaceGrantRevoked, got {other:?}"),
    }

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn grant_persists_through_registry_and_routes_per_agent() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;
    let agent = common::register_mock_agent(&kernel, "grant-agent", vec![]).await;

    // Grant a path scoped to a specific agent.
    let path = PathBuf::from("/tmp/agos-e2e-scoped");
    client
        .send_command(KernelCommand::GrantWorkspace {
            path: path.clone(),
            agent_name: Some("grant-agent".into()),
            mode: "rw".to_string(),
        })
        .await
        .expect("scoped grant");

    // Resolver-level: paths_for_agent should show the grant for THIS agent only.
    let read_paths = kernel
        .workspace_grants
        .paths_for_agent(&agent, WorkspaceGrantMode::READ);
    assert!(
        read_paths.iter().any(|p| p == &path),
        "expected scoped grant in READ list for agent: {read_paths:?}"
    );

    let write_paths = kernel
        .workspace_grants
        .paths_for_agent(&agent, WorkspaceGrantMode::READ_WRITE);
    assert!(
        write_paths.iter().any(|p| p == &path),
        "expected scoped grant in WRITE list for agent"
    );

    // An EXEC lookup must NOT include this `rw` grant.
    let exec_paths = kernel
        .workspace_grants
        .paths_for_agent(&agent, WorkspaceGrantMode::READ_WRITE_EXEC);
    assert!(
        !exec_paths.iter().any(|p| p == &path),
        "rw grant must not appear in EXEC list"
    );

    // Filtering via the Kernel::workspace_paths_for_agent helper buckets correctly.
    let buckets = kernel.workspace_paths_for_agent(&agent);
    assert!(buckets.read.contains(&path));
    assert!(buckets.writable.contains(&path));
    assert!(!buckets.executable.contains(&path));

    // Another agent (not the scoped one) sees NO grant.
    let other = common::register_mock_agent(&kernel, "other-agent", vec![]).await;
    let other_buckets = kernel.workspace_paths_for_agent(&other);
    assert!(!other_buckets.read.contains(&path));
    assert!(!other_buckets.writable.contains(&path));

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn duplicate_grant_returns_sentinel_error() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let path = PathBuf::from("/tmp/agos-e2e-dup");
    client
        .send_command(KernelCommand::GrantWorkspace {
            path: path.clone(),
            agent_name: None,
            mode: "rw".to_string(),
        })
        .await
        .expect("first grant");

    // Second grant of the same (path, agent_id) must surface as an Error.
    let resp = client
        .send_command(KernelCommand::GrantWorkspace {
            path: path.clone(),
            agent_name: None,
            mode: "rw".to_string(),
        })
        .await
        .expect("second grant");
    match resp {
        KernelResponse::Error { message } => {
            assert!(
                message.to_lowercase().contains("already exists"),
                "expected duplicate-grant message, got: {message}"
            );
        }
        other => panic!("expected Error response for duplicate grant, got {other:?}"),
    }

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn forbidden_root_grant_rejected() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let resp = client
        .send_command(KernelCommand::GrantWorkspace {
            path: PathBuf::from("/etc/agentos"),
            agent_name: None,
            mode: "rw".to_string(),
        })
        .await
        .expect("grant /etc/agentos");
    match resp {
        KernelResponse::Error { message } => {
            assert!(
                message.contains("forbidden system root"),
                "expected forbidden-root message, got: {message}"
            );
        }
        other => panic!("expected Error response for forbidden root, got {other:?}"),
    }

    kernel.shutdown();
    handle.await.unwrap();
}
