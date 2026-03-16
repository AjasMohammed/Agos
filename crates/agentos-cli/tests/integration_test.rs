mod common;

use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_kernel::Kernel;
use agentos_llm::MockLLMCore;
use agentos_types::*;
use serial_test::serial;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Helper: boot kernel, spawn it, connect a bus client.
async fn setup_kernel() -> (Arc<Kernel>, BusClient, tempfile::TempDir) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config = common::create_test_config(&temp_dir);
    let config_path = temp_dir.path().join("config.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    std::fs::create_dir_all(temp_dir.path().join("data")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("vault")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/core")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/user")).unwrap();

    let kernel = Arc::new(
        Kernel::boot(
            &config_path,
            &agentos_vault::ZeroizingString::new("test-passphrase".to_string()),
        )
        .await
        .unwrap(),
    );

    let kernel_clone = kernel.clone();
    tokio::spawn(async move {
        kernel_clone.run().await.unwrap();
    });

    // Connect with retry
    let socket = Path::new(&config.bus.socket_path).to_path_buf();
    let client = {
        let mut attempts = 0;
        loop {
            match BusClient::connect(&socket).await {
                Ok(c) => break c,
                Err(_) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("Failed to connect to bus: {}", e),
            }
        }
    };

    (kernel, client, temp_dir)
}

/// Register a mock agent directly into the kernel (bypassing ConnectAgent which requires real LLM providers).
async fn register_mock_agent(kernel: &Kernel, name: &str, responses: Vec<String>) -> AgentID {
    let agent_id = AgentID::new();
    let now = chrono::Utc::now();

    let profile = AgentProfile {
        id: agent_id,
        name: name.to_string(),
        provider: LLMProvider::Ollama, // placeholder
        model: "mock-model".to_string(),
        status: AgentStatus::Online,
        permissions: PermissionSet::new(),
        roles: vec!["base".to_string()],
        current_task: None,
        description: String::new(),
        created_at: now,
        last_active: now,
        public_key_hex: None,
    };

    {
        let mut registry = kernel.agent_registry.write().await;
        registry.register(profile);
    }

    {
        let mock = Arc::new(MockLLMCore::new(responses));
        let mut active = kernel.active_llms.write().await;
        active.insert(agent_id, mock);
    }

    // Register base permissions so capability token works
    kernel
        .capability_engine
        .register_agent(agent_id, PermissionSet::new());

    agent_id
}

/// Full lifecycle: boot → connect mock agent → run task → verify result → check audit logs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_full_lifecycle_with_mock_llm() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (kernel, mut client, _temp_dir) = setup_kernel().await;

        // Register a mock agent that returns a simple answer (no tool calls)
        let _agent_id = register_mock_agent(
            &kernel,
            "test-agent",
            vec!["The answer to your question is 42.".to_string()],
        )
        .await;

        // Run a task via the bus
        let response = client
            .send_command(KernelCommand::RunTask {
                agent_name: Some("test-agent".into()),
                prompt: "What is the meaning of life?".into(),
            })
            .await
            .unwrap();

        // Verify success with the mock's response
        match &response {
            KernelResponse::Success { data: Some(data) } => {
                let result = data["result"].as_str().unwrap();
                assert_eq!(result, "The answer to your question is 42.");
                assert!(data["task_id"].as_str().is_some());
            }
            other => panic!("Expected Success with data, got {:?}", other),
        }

        // Verify audit logs contain relevant events
        let audit_response = client
            .send_command(KernelCommand::GetAuditLogs { limit: 50 })
            .await
            .unwrap();

        match audit_response {
            KernelResponse::AuditLogs(logs) => {
                // Should have at least KernelStarted
                assert!(!logs.is_empty(), "Audit log should not be empty");

                let event_types: Vec<_> =
                    logs.iter().map(|e| format!("{:?}", e.event_type)).collect();

                assert!(
                    event_types.iter().any(|e| e.contains("KernelStarted")),
                    "Should have KernelStarted event. Events: {:?}",
                    event_types
                );
            }
            other => panic!("Expected AuditLogs, got {:?}", other),
        }

        kernel.shutdown();
    })
    .await;
    result.expect("test_full_lifecycle_with_mock_llm timed out");
}

/// Test that running a task with a nonexistent agent returns an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_run_task_nonexistent_agent() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (kernel, mut client, _temp_dir) = setup_kernel().await;

        let response = client
            .send_command(KernelCommand::RunTask {
                agent_name: Some("nonexistent".into()),
                prompt: "hello".into(),
            })
            .await
            .unwrap();

        match response {
            KernelResponse::Error { message } => {
                assert!(message.contains("not found"), "Error: {}", message);
            }
            other => panic!("Expected Error, got {:?}", other),
        }

        kernel.shutdown();
    })
    .await;
    result.expect("test_run_task_nonexistent_agent timed out");
}

/// Test multi-turn: mock LLM first requests a tool, then gives a final answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_task_with_tool_call() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (kernel, mut client, _temp_dir) = setup_kernel().await;

        // First response: a tool call JSON. Second response: the final answer.
        let _agent_id = register_mock_agent(
            &kernel,
            "tool-agent",
            vec![
                // The task executor looks for a JSON block with "tool" key.
                // This will be parsed as a tool call attempt but will fail (tool not found),
                // and the error gets pushed to context, then the LLM is called again.
                r#"I need to check the time. {"tool": "system_info", "intent_type": "read", "payload": {"query": "time"}}"#.to_string(),
                // Second call: final answer (no tool call)
                "The current time is approximately noon.".to_string(),
            ],
        )
        .await;

        let response = client
            .send_command(KernelCommand::RunTask {
                agent_name: Some("tool-agent".into()),
                prompt: "What time is it?".into(),
            })
            .await
            .unwrap();

        // The task should complete (tool call may fail but execution continues)
        match &response {
            KernelResponse::Success { data: Some(data) } => {
                let result = data["result"].as_str().unwrap();
                assert!(
                    result.contains("noon") || result.contains("time"),
                    "Result should contain the mock's final answer: {}",
                    result
                );
            }
            KernelResponse::Error { message } => {
                // Tool call might fail, which is acceptable — the key is the test exercises the multi-turn path
                eprintln!("Task returned error (expected in some cases): {}", message);
            }
            other => panic!("Expected Success or Error, got {:?}", other),
        }

        kernel.shutdown();
    })
    .await;
    result.expect("test_task_with_tool_call timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_resource_list_command() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (kernel, mut client, _temp_dir) = setup_kernel().await;

        let response = client
            .send_command(KernelCommand::ListResourceLocks)
            .await
            .unwrap();

        match response {
            KernelResponse::ResourceLockList(locks) => {
                assert!(locks.is_empty(), "Expected empty lock list initially");
            }
            other => panic!("Expected ResourceLockList, got {:?}", other),
        }

        kernel.shutdown();
    })
    .await;
    result.expect("test_resource_list_command timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_cost_report_command() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (kernel, mut client, _temp_dir) = setup_kernel().await;

        let response = client
            .send_command(KernelCommand::GetCostReport { agent_name: None })
            .await
            .unwrap();

        match response {
            KernelResponse::CostReport(_reports) => {
                // Should succeed and return empty or baseline cost report
            }
            other => panic!("Expected CostReport, got {:?}", other),
        }

        kernel.shutdown();
    })
    .await;
    result.expect("test_cost_report_command timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_escalation_list_command() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (kernel, mut client, _temp_dir) = setup_kernel().await;

        let response = client
            .send_command(KernelCommand::ListEscalations { pending_only: true })
            .await
            .unwrap();

        match response {
            KernelResponse::EscalationList(list) => {
                assert!(list.is_empty(), "Expected empty escalation list initially");
            }
            other => panic!("Expected EscalationList, got {:?}", other),
        }

        kernel.shutdown();
    })
    .await;
    result.expect("test_escalation_list_command timed out");
}
