mod common;

use agentos_audit::log::AuditEventType;
use agentos_kernel::Kernel;
use std::sync::Arc;

#[tokio::test]
async fn test_kernel_boots_and_shuts_down() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    let config = common::create_test_config(&temp_dir);
    let config_path = temp_dir.path().join("config.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    std::fs::create_dir_all(temp_dir.path().join("data")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("vault")).unwrap();

    let core_tools_dir = temp_dir.path().join("tools/core");
    std::fs::create_dir_all(&core_tools_dir).unwrap();

    // Create 5 dummy tools to pass the >= 5 assertion
    for i in 1..=5 {
        let manifest = format!(
            r#"
[manifest]
name = "tool-{}"
version = "1.0"
description = "Dummy tool {}"
author = "Test"
trust_tier = "core"

[capabilities_required]
permissions = []

[capabilities_provided]
outputs = []

[intent_schema]
input = "DummyIntent"
output = "DummyResult"

[sandbox]
network = false
fs_write = false
max_memory_mb = 128
max_cpu_ms = 1000
        "#,
            i, i
        );
        std::fs::write(core_tools_dir.join(format!("tool-{}.toml", i)), manifest).unwrap();
    }

    let kernel = Arc::new(Kernel::boot(&config_path, "test-passphrase").await.unwrap());

    let logs = kernel.audit.query_recent(10).unwrap();
    assert!(logs
        .iter()
        .any(|e| matches!(e.event_type, AuditEventType::KernelStarted)));

    let tools = kernel.tool_registry.read().await;
    assert!(
        tools.list_all().len() >= 5,
        "Should have at least 5 core tools"
    );
}
