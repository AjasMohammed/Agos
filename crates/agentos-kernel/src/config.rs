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
    pub llm: LlmSettings,
    #[serde(default)]
    pub memory: MemorySettings,
    #[serde(default)]
    pub routing: RoutingConfig,
    /// Token budget for context compilation. Optional; defaults to standard
    /// allocation if omitted from config TOML.
    #[serde(default)]
    pub context_budget: agentos_types::TokenBudget,
    #[serde(default)]
    pub health_monitor: HealthMonitorConfig,
    #[serde(default)]
    pub preflight: PreflightConfig,
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
    #[serde(default)]
    pub context_window_token_budget: usize,
    #[serde(default = "default_health_port")]
    pub health_port: u16,
    /// Maximum commands per second per agent (across all connections). 0 = unlimited.
    #[serde(default = "default_per_agent_rate_limit")]
    pub per_agent_rate_limit: u32,
}

fn default_health_port() -> u16 {
    9091
}

fn default_per_agent_rate_limit() -> u32 {
    100
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecretsSettings {
    pub vault_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuditSettings {
    pub log_path: String,
    /// Maximum number of audit log rows to retain. Older entries are pruned when the
    /// TimeoutChecker runs its periodic sweep. `0` means unlimited (default).
    #[serde(default)]
    pub max_audit_entries: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolsSettings {
    pub core_tools_dir: String,
    pub user_tools_dir: String,
    pub data_dir: String,
    /// Optional path to a JSON certificate revocation list (array of hex pubkey strings).
    /// Tools signed by revoked keys are rejected at registration time.
    #[serde(default)]
    pub crl_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BusSettings {
    pub socket_path: String,
    /// Optional TLS configuration for TCP transport.
    /// When present, the kernel also listens on a TCP port with TLS encryption.
    #[serde(default)]
    pub tls: Option<TlsSettings>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TlsSettings {
    /// TCP address to bind (e.g. "0.0.0.0:9443")
    pub bind_addr: String,
    /// Path to PEM-encoded TLS certificate chain
    pub cert_path: String,
    /// Path to PEM-encoded TLS private key
    pub key_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OllamaSettings {
    pub host: String,
    pub default_model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct LlmSettings {
    /// Base URL for custom/OpenAI-compatible providers.
    #[serde(default)]
    pub custom_base_url: Option<String>,
    /// Optional OpenAI API base URL override.
    #[serde(default)]
    pub openai_base_url: Option<String>,
    /// Optional Anthropic endpoint base URL (documented for deployment parity).
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    /// Optional Gemini endpoint base URL (documented for deployment parity).
    #[serde(default)]
    pub gemini_base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemorySettings {
    #[serde(default = "default_model_cache_dir")]
    pub model_cache_dir: String,
    #[serde(default)]
    pub extraction: crate::memory_extraction::ExtractionConfig,
    #[serde(default)]
    pub consolidation: crate::consolidation::ConsolidationConfig,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            model_cache_dir: default_model_cache_dir(),
            extraction: crate::memory_extraction::ExtractionConfig::default(),
            consolidation: crate::consolidation::ConsolidationConfig::default(),
        }
    }
}

fn default_model_cache_dir() -> String {
    "models".to_string()
}

/// Configuration for boot-time pre-flight system health checks.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PreflightConfig {
    /// Minimum free disk space in MB on the data directory partition.
    /// Boot fails if free space is below this threshold. Set to 0 to disable.
    #[serde(default = "default_min_free_disk_mb")]
    pub min_free_disk_mb: u64,
    /// Whether to perform a write test on database parent directories.
    #[serde(default = "default_check_db_writable")]
    pub check_db_writable: bool,
}

impl Default for PreflightConfig {
    fn default() -> Self {
        Self {
            min_free_disk_mb: default_min_free_disk_mb(),
            check_db_writable: default_check_db_writable(),
        }
    }
}

fn default_min_free_disk_mb() -> u64 {
    100
}

fn default_check_db_writable() -> bool {
    true
}

/// Configuration for the periodic system health monitoring loop.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthMonitorConfig {
    pub enabled: bool,
    pub check_interval_secs: u64,
    pub thresholds: HealthThresholds,
}

impl Default for HealthMonitorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_secs: 30,
            thresholds: HealthThresholds::default(),
        }
    }
}

/// Threshold values for each health metric. Percentages are 0–100.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthThresholds {
    pub cpu_warning_percent: f32,
    pub memory_warning_percent: f32,
    pub disk_warning_percent: f32,
    pub disk_critical_percent: f32,
    pub gpu_vram_warning_percent: f32,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            cpu_warning_percent: 85.0,
            memory_warning_percent: 80.0,
            disk_warning_percent: 85.0,
            disk_critical_percent: 95.0,
            gpu_vram_warning_percent: 90.0,
        }
    }
}

/// Load kernel configuration from a TOML file.
pub fn load_config(path: &std::path::Path) -> Result<KernelConfig, anyhow::Error> {
    let content = std::fs::read_to_string(path)?;
    let mut config: KernelConfig = toml::from_str(&content)?;
    apply_env_overrides(&mut config);
    warn_on_tmp_paths(&config);
    Ok(config)
}

fn apply_env_overrides(config: &mut KernelConfig) {
    if let Ok(host) = std::env::var("AGENTOS_OLLAMA_HOST") {
        if !host.trim().is_empty() {
            config.ollama.host = host;
        }
    }

    if let Ok(url) = std::env::var("AGENTOS_LLM_URL") {
        if !url.trim().is_empty() {
            config.llm.custom_base_url = Some(url);
        }
    }

    if let Ok(url) = std::env::var("AGENTOS_OPENAI_BASE_URL") {
        if !url.trim().is_empty() {
            config.llm.openai_base_url = Some(url);
        }
    }
}

fn warn_on_tmp_paths(config: &KernelConfig) {
    let runtime_paths = [
        ("secrets.vault_path", config.secrets.vault_path.as_str()),
        ("audit.log_path", config.audit.log_path.as_str()),
        ("tools.core_tools_dir", config.tools.core_tools_dir.as_str()),
        ("tools.user_tools_dir", config.tools.user_tools_dir.as_str()),
        ("tools.data_dir", config.tools.data_dir.as_str()),
        ("bus.socket_path", config.bus.socket_path.as_str()),
    ];

    for (name, path) in runtime_paths {
        if is_tmp_path(path) {
            tracing::warn!(
                config_key = %name,
                path = %path,
                "Runtime path points to a temporary location; use persistent storage in production"
            );
        }
    }

    // Only warn for model_cache_dir when it is absolute; relative paths inherit
    // their safety from tools.data_dir, which is already checked above.
    let model_cache = config.memory.model_cache_dir.as_str();
    if std::path::Path::new(model_cache).is_absolute() && is_tmp_path(model_cache) {
        tracing::warn!(
            config_key = "memory.model_cache_dir",
            path = %model_cache,
            "Runtime path points to a temporary location; use persistent storage in production"
        );
    }
}

fn is_tmp_path(path: &str) -> bool {
    let p = std::path::Path::new(path);
    p.starts_with("/tmp") || p.starts_with("/var/tmp")
}
