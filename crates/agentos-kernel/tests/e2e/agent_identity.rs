use crate::common;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::LLMProvider;
use serial_test::serial;

/// Connect an agent, disconnect it, then reconnect with the same name + provider + model.
/// The returned agent_id must be identical — identity is preserved across the reconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_reconnect_same_provider_model_reuses_agent_id() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    // First connection
    let first_id = match client
        .send_command(KernelCommand::ConnectAgent {
            name: "identity-agent".to_string(),
            provider: LLMProvider::Ollama,
            model: "llama3.2".to_string(),
            base_url: Some("http://localhost:11434".to_string()),
            roles: vec![],
            test_mode: false,
            extra_permissions: vec![],
        })
        .await
        .expect("first ConnectAgent")
    {
        KernelResponse::Success { data: Some(d) } => d["agent_id"].as_str().unwrap().to_string(),
        other => panic!("Expected Success on first connect, got: {other:?}"),
    };

    // Disconnect
    let first_uuid: agentos_types::AgentID = first_id.parse().unwrap();
    match client
        .send_command(KernelCommand::DisconnectAgent {
            agent_id: first_uuid,
        })
        .await
        .expect("DisconnectAgent")
    {
        KernelResponse::Success { .. } => {}
        other => panic!("Expected Success on disconnect, got: {other:?}"),
    }

    // Reconnect with same name + provider + model
    let second_id = match client
        .send_command(KernelCommand::ConnectAgent {
            name: "identity-agent".to_string(),
            provider: LLMProvider::Ollama,
            model: "llama3.2".to_string(),
            base_url: Some("http://localhost:11434".to_string()),
            roles: vec![],
            test_mode: false,
            extra_permissions: vec![],
        })
        .await
        .expect("second ConnectAgent")
    {
        KernelResponse::Success { data: Some(d) } => d["agent_id"].as_str().unwrap().to_string(),
        other => panic!("Expected Success on second connect, got: {other:?}"),
    };

    assert_eq!(
        first_id, second_id,
        "agent_id must be identical on reconnect with same provider+model"
    );

    // Verify AgentReconnected audit entry was written for the second connect
    let reconnect_entries = kernel
        .audit
        .query_by_type(agentos_audit::AuditEventType::AgentReconnected, 10)
        .unwrap();
    assert_eq!(
        reconnect_entries.len(),
        1,
        "expected exactly 1 AgentReconnected audit entry, got {}",
        reconnect_entries.len()
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Reconnecting with the same name but a different model issues a new UUID.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_reconnect_different_model_issues_new_agent_id() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let first_id = match client
        .send_command(KernelCommand::ConnectAgent {
            name: "model-switch-agent".to_string(),
            provider: LLMProvider::Ollama,
            model: "llama3.2".to_string(),
            base_url: Some("http://localhost:11434".to_string()),
            roles: vec![],
            test_mode: false,
            extra_permissions: vec![],
        })
        .await
        .expect("first ConnectAgent")
    {
        KernelResponse::Success { data: Some(d) } => d["agent_id"].as_str().unwrap().to_string(),
        other => panic!("Expected Success on first connect, got: {other:?}"),
    };

    let first_uuid: agentos_types::AgentID = first_id.parse().unwrap();
    match client
        .send_command(KernelCommand::DisconnectAgent {
            agent_id: first_uuid,
        })
        .await
        .expect("DisconnectAgent")
    {
        KernelResponse::Success { .. } => {}
        other => panic!("Expected Success on disconnect, got: {other:?}"),
    }

    // Reconnect with same name but different model
    let second_id = match client
        .send_command(KernelCommand::ConnectAgent {
            name: "model-switch-agent".to_string(),
            provider: LLMProvider::Ollama,
            model: "mistral".to_string(),
            base_url: Some("http://localhost:11434".to_string()),
            roles: vec![],
            test_mode: false,
            extra_permissions: vec![],
        })
        .await
        .expect("second ConnectAgent different model")
    {
        KernelResponse::Success { data: Some(d) } => d["agent_id"].as_str().unwrap().to_string(),
        other => panic!("Expected Success on second connect, got: {other:?}"),
    };

    assert_ne!(
        first_id, second_id,
        "agent_id must differ when model changes"
    );

    kernel.shutdown();
    handle.await.unwrap();
}
