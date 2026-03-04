use agentos_kernel::config::{KernelConfig, KernelSettings, SecretsSettings, AuditSettings, ToolsSettings, BusSettings, OllamaSettings};

pub fn create_test_config(temp_dir: &tempfile::TempDir) -> KernelConfig {
    KernelConfig {
        kernel: KernelSettings {
            max_concurrent_tasks: 4,
            default_task_timeout_secs: 60,
            context_window_max_entries: 100,
        },
        routing: Default::default(),
        secrets: SecretsSettings {
            vault_path: temp_dir.path().join("vault/secrets.db").to_string_lossy().to_string(),
        },
        audit: AuditSettings {
            log_path: temp_dir.path().join("data/audit.db").to_string_lossy().to_string(),
        },
        tools: ToolsSettings {
            core_tools_dir: temp_dir.path().join("tools/core").to_string_lossy().to_string(),
            user_tools_dir: temp_dir.path().join("tools/user").to_string_lossy().to_string(),
            data_dir: temp_dir.path().join("data").to_string_lossy().to_string(),
        },
        bus: BusSettings {
            socket_path: temp_dir.path().join("agentos.sock").to_string_lossy().to_string(),
        },
        ollama: OllamaSettings {
            host: "http://localhost:11434".to_string(),
            default_model: "llama3.2".to_string(),
        },
    }
}
