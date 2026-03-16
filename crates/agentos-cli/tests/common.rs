use agentos_kernel::config::{
    AuditSettings, BusSettings, HealthMonitorConfig, KernelConfig, KernelSettings, LlmSettings,
    MemorySettings, OllamaSettings, SecretsSettings, ToolsSettings,
};

/// Returns a persistent model cache directory so the ~23MB embedding model is
/// downloaded once and reused across test runs (instead of per-TempDir).
fn shared_model_cache_dir() -> String {
    let cache_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-model-cache");
    std::fs::create_dir_all(&cache_dir).expect("create shared model cache dir");
    cache_dir.to_string_lossy().to_string()
}

pub fn create_test_config(temp_dir: &tempfile::TempDir) -> KernelConfig {
    KernelConfig {
        kernel: KernelSettings {
            max_concurrent_tasks: 4,
            default_task_timeout_secs: 60,
            context_window_max_entries: 100,
            context_window_token_budget: 0,
            health_port: 0,          // 0 = disabled in tests
            per_agent_rate_limit: 0, // 0 = unlimited in tests
        },
        routing: Default::default(),
        secrets: SecretsSettings {
            vault_path: temp_dir
                .path()
                .join("vault/secrets.db")
                .to_string_lossy()
                .to_string(),
        },
        audit: AuditSettings {
            log_path: temp_dir
                .path()
                .join("data/audit.db")
                .to_string_lossy()
                .to_string(),
            max_audit_entries: 0, // 0 = unlimited in tests
        },
        tools: ToolsSettings {
            core_tools_dir: temp_dir
                .path()
                .join("tools/core")
                .to_string_lossy()
                .to_string(),
            user_tools_dir: temp_dir
                .path()
                .join("tools/user")
                .to_string_lossy()
                .to_string(),
            data_dir: temp_dir.path().join("data").to_string_lossy().to_string(),
            crl_path: None,
        },
        bus: BusSettings {
            socket_path: temp_dir
                .path()
                .join("agentos.sock")
                .to_string_lossy()
                .to_string(),
            tls: None,
        },
        ollama: OllamaSettings {
            host: "http://localhost:11434".to_string(),
            default_model: "llama3.2".to_string(),
        },
        llm: LlmSettings::default(),
        memory: MemorySettings {
            model_cache_dir: shared_model_cache_dir(),
            extraction: Default::default(),
            consolidation: Default::default(),
        },
        context_budget: Default::default(),
        health_monitor: HealthMonitorConfig::default(),
    }
}
