mod common;

use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_kernel::Kernel;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

async fn setup_kernel_and_client(temp_dir: &tempfile::TempDir) -> (Arc<Kernel>, BusClient) {
    let config = common::create_test_config(temp_dir);
    let config_path = temp_dir.path().join("config.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    std::fs::create_dir_all(temp_dir.path().join("data")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("vault")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/core")).unwrap();

    let kernel = Arc::new(Kernel::boot(&config_path, "test-passphrase").await.unwrap());

    let kernel_clone = kernel.clone();
    tokio::spawn(async move {
        kernel_clone.run().await.unwrap();
    });

    // Connect with retry
    let client = {
        let socket = Path::new(&config.bus.socket_path);
        let mut attempts = 0;
        loop {
            match BusClient::connect(socket).await {
                Ok(c) => break c,
                Err(_) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("Failed to connect to bus after retries: {}", e),
            }
        }
    };

    (kernel, client)
}

const TEST_PIPELINE_YAML: &str = r#"
name: "integration-test-pipeline"
version: "1.0.0"
description: "Pipeline for integration testing."
steps:
  - id: greet
    agent: researcher
    task: "Say hello about: {{input}}"
    output_var: greeting
  - id: enhance
    agent: analyst
    task: "Enhance this: {{greeting}}"
    output_var: enhanced
    depends_on: [greet]
output: enhanced
"#;

#[tokio::test]
async fn test_pipeline_install_list_remove() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let (_kernel, mut client) = setup_kernel_and_client(&temp_dir).await;

    // Install pipeline
    let response = client
        .send_command(KernelCommand::InstallPipeline {
            yaml: TEST_PIPELINE_YAML.to_string(),
        })
        .await
        .unwrap();

    match &response {
        KernelResponse::Success { data: Some(data) } => {
            assert_eq!(
                data.get("name").and_then(|v| v.as_str()),
                Some("integration-test-pipeline")
            );
            assert_eq!(data.get("version").and_then(|v| v.as_str()), Some("1.0.0"));
            assert_eq!(data.get("steps").and_then(|v| v.as_u64()), Some(2));
        }
        resp => panic!("Expected Success, got {:?}", resp),
    }

    // List pipelines
    let response = client
        .send_command(KernelCommand::PipelineList)
        .await
        .unwrap();

    match &response {
        KernelResponse::PipelineList(list) => {
            assert_eq!(list.len(), 1);
            assert_eq!(
                list[0].get("name").and_then(|v| v.as_str()),
                Some("integration-test-pipeline")
            );
            assert_eq!(list[0].get("step_count").and_then(|v| v.as_u64()), Some(2));
        }
        resp => panic!("Expected PipelineList, got {:?}", resp),
    }

    // Remove pipeline
    let response = client
        .send_command(KernelCommand::RemovePipeline {
            name: "integration-test-pipeline".to_string(),
        })
        .await
        .unwrap();
    assert!(matches!(response, KernelResponse::Success { .. }));

    // List should be empty
    let response = client
        .send_command(KernelCommand::PipelineList)
        .await
        .unwrap();
    match &response {
        KernelResponse::PipelineList(list) => assert_eq!(list.len(), 0),
        resp => panic!("Expected PipelineList, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_pipeline_install_invalid_yaml_fails() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let (_kernel, mut client) = setup_kernel_and_client(&temp_dir).await;

    let response = client
        .send_command(KernelCommand::InstallPipeline {
            yaml: "this is not valid yaml: [[[".to_string(),
        })
        .await
        .unwrap();

    assert!(matches!(response, KernelResponse::Error { .. }));
}

#[tokio::test]
async fn test_pipeline_run_nonexistent_fails() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let (_kernel, mut client) = setup_kernel_and_client(&temp_dir).await;

    let response = client
        .send_command(KernelCommand::RunPipeline {
            name: "nonexistent".to_string(),
            input: "test".to_string(),
            detach: false,
        })
        .await
        .unwrap();

    assert!(matches!(response, KernelResponse::Error { .. }));
}

#[tokio::test]
async fn test_pipeline_remove_nonexistent_fails() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let (_kernel, mut client) = setup_kernel_and_client(&temp_dir).await;

    let response = client
        .send_command(KernelCommand::RemovePipeline {
            name: "nonexistent".to_string(),
        })
        .await
        .unwrap();

    assert!(matches!(response, KernelResponse::Error { .. }));
}

#[tokio::test]
async fn test_pipeline_status_invalid_run_id_fails() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let (_kernel, mut client) = setup_kernel_and_client(&temp_dir).await;

    let response = client
        .send_command(KernelCommand::PipelineStatus {
            name: "test".to_string(),
            run_id: "not-a-uuid".to_string(),
        })
        .await
        .unwrap();

    assert!(matches!(response, KernelResponse::Error { .. }));
}

#[tokio::test]
async fn test_pipeline_reinstall_updates_version() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let (_kernel, mut client) = setup_kernel_and_client(&temp_dir).await;

    // Install v1
    let yaml_v1 = r#"
name: "versioned-pipe"
version: "1.0.0"
steps:
  - id: step1
    agent: test
    task: "v1 task"
"#;
    let response = client
        .send_command(KernelCommand::InstallPipeline {
            yaml: yaml_v1.to_string(),
        })
        .await
        .unwrap();
    assert!(matches!(response, KernelResponse::Success { .. }));

    // Install v2 (same name)
    let yaml_v2 = r#"
name: "versioned-pipe"
version: "2.0.0"
steps:
  - id: step1
    agent: test
    task: "v2 task"
  - id: step2
    agent: test2
    task: "additional step"
    depends_on: [step1]
"#;
    let response = client
        .send_command(KernelCommand::InstallPipeline {
            yaml: yaml_v2.to_string(),
        })
        .await
        .unwrap();
    assert!(matches!(response, KernelResponse::Success { .. }));

    // List should show v2
    let response = client
        .send_command(KernelCommand::PipelineList)
        .await
        .unwrap();
    match &response {
        KernelResponse::PipelineList(list) => {
            assert_eq!(list.len(), 1);
            assert_eq!(
                list[0].get("version").and_then(|v| v.as_str()),
                Some("2.0.0")
            );
            assert_eq!(list[0].get("step_count").and_then(|v| v.as_u64()), Some(2));
        }
        resp => panic!("Expected PipelineList, got {:?}", resp),
    }
}
