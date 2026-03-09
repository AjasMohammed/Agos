mod common;

use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_kernel::Kernel;
use agentos_types::secret::SecretScope;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn test_secrets_full_lifecycle() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    let config = common::create_test_config(&temp_dir);
    let config_path = temp_dir.path().join("config.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    std::fs::create_dir_all(temp_dir.path().join("data")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("vault")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/core")).unwrap();

    let kernel = Arc::new(Kernel::boot(&config_path, "test-passphrase").await.unwrap());

    // Spawn kernel in background
    let kernel_clone = kernel.clone();
    tokio::spawn(async move {
        kernel_clone.run().await.unwrap();
    });

    // Connect with retry — wait for bus server to start
    let mut client = {
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

    // 1. Set a secret
    let response = client
        .send_command(KernelCommand::SetSecret {
            name: "TEST_KEY".into(),
            value: "super-secret-123".into(),
            scope: SecretScope::Global,
        })
        .await
        .unwrap();
    match response {
        KernelResponse::Success { .. } => {}
        resp => panic!("Expected Success, got {:?}", resp),
    }

    // 2. List secrets — value should NOT appear
    let response = client
        .send_command(KernelCommand::ListSecrets)
        .await
        .unwrap();
    match response {
        KernelResponse::SecretList(secrets) => {
            // Filter out internal keys (e.g. __internal_hmac_signing_key)
            let user_secrets: Vec<_> = secrets.iter().filter(|s| !s.name.starts_with("__internal_")).collect();
            assert_eq!(user_secrets.len(), 1);
            assert_eq!(user_secrets[0].name, "TEST_KEY");
            // No value field exists on SecretMetadata — this is by design
        }
        _ => panic!("Wrong response type"),
    }

    // 3. Rotate
    let response = client
        .send_command(KernelCommand::RotateSecret {
            name: "TEST_KEY".into(),
            new_value: "new-secret-456".into(),
        })
        .await
        .unwrap();
    match response {
        KernelResponse::Success { .. } => {}
        resp => panic!("Expected Success on RotateSecret, got {:?}", resp),
    }

    // 4. Revoke
    let response = client
        .send_command(KernelCommand::RevokeSecret {
            name: "TEST_KEY".into(),
        })
        .await
        .unwrap();
    assert!(matches!(response, KernelResponse::Success { .. }));

    // 5. List should be empty
    let response = client
        .send_command(KernelCommand::ListSecrets)
        .await
        .unwrap();
    match response {
        KernelResponse::SecretList(secrets) => {
            let user_secrets: Vec<_> = secrets.iter().filter(|s| !s.name.starts_with("__internal_")).collect();
            assert_eq!(user_secrets.len(), 0);
        }
        _ => panic!("Wrong response type"),
    }
}
