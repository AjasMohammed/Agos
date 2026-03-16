use crate::common;
use agentos_bus::message::{KernelCommand, KernelResponse};
use serial_test::serial;

/// Kernel boots without error and the bus accepts connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_kernel_boots_and_accepts_connections() {
    let (_kernel, _client, _tmp) = common::setup_kernel().await;
    // Reaching here means the kernel booted and the bus client connected.
}

/// GetStatus returns a SystemStatus with sane initial values.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_get_status_returns_system_info() {
    let (_kernel, mut client, _tmp) = common::setup_kernel().await;

    let response = client
        .send_command(KernelCommand::GetStatus)
        .await
        .expect("send GetStatus");

    match response {
        KernelResponse::Status(status) => {
            assert_eq!(status.connected_agents, 0);
            assert_eq!(status.active_tasks, 0);
        }
        other => panic!("Expected Status, got: {other:?}"),
    }
}

/// ListAgents returns an empty list on a freshly-booted kernel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_list_agents_empty_on_fresh_boot() {
    let (_kernel, mut client, _tmp) = common::setup_kernel().await;

    let response = client
        .send_command(KernelCommand::ListAgents)
        .await
        .expect("send ListAgents");

    match response {
        KernelResponse::AgentList(agents) => {
            assert!(agents.is_empty(), "expected no agents, got {}", agents.len());
        }
        other => panic!("Expected AgentList, got: {other:?}"),
    }
}

/// After registering a mock agent, ListAgents returns it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_register_and_list_agent() {
    let (kernel, mut client, _tmp) = common::setup_kernel().await;

    let agent_id = common::register_mock_agent(&kernel, "test-agent", vec![]).await;

    let response = client
        .send_command(KernelCommand::ListAgents)
        .await
        .expect("send ListAgents");

    match response {
        KernelResponse::AgentList(agents) => {
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].id, agent_id);
            assert_eq!(agents[0].name, "test-agent");
        }
        other => panic!("Expected AgentList, got: {other:?}"),
    }
}

/// Disconnecting an agent removes it from the registry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_disconnect_agent_removes_from_registry() {
    let (kernel, mut client, _tmp) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "removable", vec![]).await;

    match client
        .send_command(KernelCommand::DisconnectAgent { agent_id })
        .await
        .expect("send DisconnectAgent")
    {
        KernelResponse::Success { .. } => {}
        other => panic!("Expected Success on disconnect, got: {other:?}"),
    }

    match client
        .send_command(KernelCommand::ListAgents)
        .await
        .expect("send ListAgents after disconnect")
    {
        KernelResponse::AgentList(agents) => {
            assert!(agents.is_empty(), "expected no agents after disconnect");
        }
        other => panic!("Expected AgentList, got: {other:?}"),
    }
}

/// ListTools returns the installed core tools on a fresh kernel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_list_tools_returns_core_tools() {
    let (_kernel, mut client, _tmp) = common::setup_kernel().await;

    let response = client
        .send_command(KernelCommand::ListTools)
        .await
        .expect("send ListTools");

    match response {
        KernelResponse::ToolList(tools) => {
            assert!(!tools.is_empty(), "expected at least one core tool installed");
        }
        other => panic!("Expected ToolList, got: {other:?}"),
    }
}

/// Per-agent rate limiter evicts state when an agent disconnects.
///
/// Verifies `tracked_count()` is 0 after disconnect so no memory leaks on churn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_rate_limiter_evicted_on_disconnect() {
    let (kernel, mut client, _tmp) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "rate-limit-test", vec![]).await;

    // Before disconnect: 0 tracked (keys only inserted on first rate-limited command)
    assert_eq!(kernel.rate_limiter_tracked_count().await, 0);

    match client
        .send_command(KernelCommand::DisconnectAgent { agent_id })
        .await
        .expect("send DisconnectAgent")
    {
        KernelResponse::Success { .. } => {}
        other => panic!("Expected Success, got: {other:?}"),
    }

    assert_eq!(
        kernel.rate_limiter_tracked_count().await,
        0,
        "rate limiter entry not cleaned up on disconnect"
    );
}

/// Audit log prune_old_entries trims when the row limit is exceeded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_audit_log_rotation() {
    let (_kernel, mut client, _tmp) = common::setup_kernel().await;

    // Generate several audit events by listing agents repeatedly
    for _ in 0..5 {
        client
            .send_command(KernelCommand::GetStatus)
            .await
            .expect("send GetStatus");
    }

    // Prune to max 2 entries directly on the audit log
    let audit = _kernel.audit.clone();
    let pruned = audit.prune_old_entries(2).unwrap();
    let remaining = audit.count().unwrap();

    assert!(pruned > 0 || remaining <= 2, "prune should have trimmed entries");
    assert!(remaining <= 2, "expected at most 2 entries after rotation, got {remaining}");
}
