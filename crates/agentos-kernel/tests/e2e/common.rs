use agentos_bus::client::BusClient;
use agentos_kernel::config::{
    AuditSettings, BusSettings, HealthMonitorConfig, KernelConfig, KernelSettings, LlmSettings,
    MemorySettings, OllamaSettings, PreflightConfig, SecretsSettings, ToolsSettings,
};
use agentos_kernel::Kernel;
use agentos_llm::MockLLMCore;
use agentos_types::*;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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
            health_port: 0,
            per_agent_rate_limit: 0,
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
            max_audit_entries: 0,
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
        preflight: PreflightConfig::default(),
    }
}

/// Boot the kernel into a temp directory, spawn the run loop, and connect a
/// BusClient. Returns `(kernel, client, temp_dir, run_handle)` — keep
/// `temp_dir` alive for the duration of the test. Await `run_handle` after
/// triggering shutdown to ensure the supervisor loop has fully exited (and
/// written any shutdown audit entries) before making assertions.
pub async fn setup_kernel() -> (
    Arc<Kernel>,
    BusClient,
    tempfile::TempDir,
    tokio::task::JoinHandle<()>,
) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config = create_test_config(&temp_dir);
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
    let run_handle = tokio::spawn(async move {
        kernel_clone.run().await.unwrap();
    });

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
                Err(e) => panic!("Failed to connect to kernel bus: {e}"),
            }
        }
    };

    (kernel, client, temp_dir, run_handle)
}

/// Register a mock agent directly into the kernel registry and wire up a
/// deterministic `MockLLMCore` for it.
pub async fn register_mock_agent(kernel: &Kernel, name: &str, responses: Vec<String>) -> AgentID {
    let agent_id = AgentID::new();
    let now = chrono::Utc::now();

    let profile = AgentProfile {
        id: agent_id,
        name: name.to_string(),
        provider: LLMProvider::Ollama,
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

    kernel.agent_registry.write().await.register(profile);
    kernel
        .active_llms
        .write()
        .await
        .insert(agent_id, Arc::new(MockLLMCore::new(responses)));

    agent_id
}
