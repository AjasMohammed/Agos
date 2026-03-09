use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KernelConfig {
    pub kernel: KernelSettings,
    pub secrets: SecretsSettings,
    pub audit: AuditSettings,
    pub tools: ToolsSettings,
    pub bus: BusSettings,
    pub ollama: OllamaSettings,
    #[serde(default)]
    pub memory: MemorySettings,
    #[serde(default)]
    pub routing: RoutingConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RoutingConfig {
    #[serde(default)]
    pub strategy: crate::router::RoutingStrategy,
    #[serde(default)]
    pub rules: Vec<crate::router::RoutingRule>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KernelSettings {
    pub max_concurrent_tasks: usize,
    pub default_task_timeout_secs: u64,
    pub context_window_max_entries: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecretsSettings {
    pub vault_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuditSettings {
    pub log_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolsSettings {
    pub core_tools_dir: String,
    pub user_tools_dir: String,
    pub data_dir: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BusSettings {
    pub socket_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OllamaSettings {
    pub host: String,
    pub default_model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemorySettings {
    #[serde(default = "default_model_cache_dir")]
    pub model_cache_dir: String,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            model_cache_dir: default_model_cache_dir(),
        }
    }
}

fn default_model_cache_dir() -> String {
    "models".to_string()
}

/// Load kernel configuration from a TOML file.
pub fn load_config(path: &std::path::Path) -> Result<KernelConfig, anyhow::Error> {
    let content = std::fs::read_to_string(path)?;
    let config: KernelConfig = toml::from_str(&content)?;
    Ok(config)
}
