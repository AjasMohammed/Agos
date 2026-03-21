use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Controls when tools are executed in a sandbox child process vs in-process.
///
/// - `TrustAware` (default): Core-tier tools run in-process (shared memory stores,
///   zero fork overhead); Community/Verified tools run sandboxed with seccomp+rlimits.
/// - `Always`: Every sandbox-eligible tool runs in a child process (legacy behavior).
/// - `Never`: No sandboxing — development/testing only, **not production-safe**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPolicy {
    /// Core tools in-process, Community/Verified tools sandboxed.
    #[default]
    TrustAware,
    /// All sandbox-eligible tools run in sandbox children.
    Always,
    /// No sandboxing at all (development only).
    Never,
}

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
    #[serde(default)]
    pub logging: LoggingSettings,
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
    /// SQLite database path for persisted kernel runtime state
    /// (scheduler queue, escalations, cost snapshots).
    #[serde(default = "default_state_db_path")]
    pub state_db_path: String,
    #[serde(default)]
    pub task_limits: TaskLimitsConfig,
    #[serde(default)]
    pub tool_calls: ToolCallSettings,
    #[serde(default)]
    pub tool_execution: ToolExecutionConfig,
    /// Limits applied when a task runs in autonomous mode (`task.autonomous = true`).
    /// These replace the normal per-complexity caps so long-running agents can
    /// work to natural completion without hitting artificial ceilings.
    #[serde(default)]
    pub autonomous_mode: AutonomousModeConfig,
    #[serde(default = "default_health_port")]
    pub health_port: u16,
    /// Maximum commands per second per agent (across all connections). 0 = unlimited.
    #[serde(default = "default_per_agent_rate_limit")]
    pub per_agent_rate_limit: u32,
    /// Event broadcast channel configuration.
    #[serde(default)]
    pub events: EventChannelConfig,
    /// Controls when tools are executed in sandbox child processes vs in-process.
    #[serde(default)]
    pub sandbox_policy: SandboxPolicy,
    /// Maximum concurrent sandbox child processes. Prevents thread/process
    /// exhaustion when multiple Community/Verified tools run in parallel.
    /// Default: number of logical CPUs (minimum 2).
    #[serde(default = "default_max_concurrent_sandbox_children")]
    pub max_concurrent_sandbox_children: usize,
}

/// Per-tool output and runtime limits applied at context injection time.
///
/// These limits protect the agentic loop from misbehaving tools without
/// terminating the overall task — a truncated or timed-out tool call is
/// surfaced as an error message in the agent's context so it can adapt.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolExecutionConfig {
    /// Maximum bytes allowed in a single tool's serialized output before it is
    /// truncated. Prevents OOM and token-budget overruns from large payloads.
    /// The truncation marker informs the agent it received partial output.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Wall-clock timeout in seconds for in-process (non-sandboxed) tool calls.
    /// Sandboxed tools already have their own per-manifest timeout via
    /// `sandbox.max_cpu_ms`; this setting only covers the in-process fallback path.
    #[serde(default = "default_tool_timeout_seconds")]
    pub default_timeout_seconds: u64,
}

impl Default for ToolExecutionConfig {
    fn default() -> Self {
        Self {
            max_output_bytes: default_max_output_bytes(),
            default_timeout_seconds: default_tool_timeout_seconds(),
        }
    }
}

/// Configuration for the internal event broadcast channel.
///
/// The channel connects event producers (kernel subsystems) to the
/// `EventDispatcher` consumer task.  A larger capacity reduces the chance of
/// events being dropped under burst load at the cost of additional memory.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventChannelConfig {
    /// Number of events the channel can buffer before producers start dropping.
    /// Tune this up when observing `EventChannelFull` audit entries under load.
    #[serde(default = "default_event_channel_capacity")]
    pub channel_capacity: usize,
}

impl Default for EventChannelConfig {
    fn default() -> Self {
        Self {
            channel_capacity: default_event_channel_capacity(),
        }
    }
}

fn default_event_channel_capacity() -> usize {
    1024
}

fn default_max_output_bytes() -> usize {
    262_144 // 256 KiB
}

fn default_tool_timeout_seconds() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallSettings {
    #[serde(default = "default_allow_parallel_tool_calls")]
    pub allow_parallel: bool,
    #[serde(default = "default_max_parallel_tool_calls")]
    pub max_parallel: usize,
}

impl Default for ToolCallSettings {
    fn default() -> Self {
        Self {
            allow_parallel: default_allow_parallel_tool_calls(),
            max_parallel: default_max_parallel_tool_calls(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskLimitsConfig {
    #[serde(default = "default_max_iterations_low")]
    pub max_iterations_low: u32,
    #[serde(default = "default_max_iterations_medium")]
    pub max_iterations_medium: u32,
    #[serde(default = "default_max_iterations_high")]
    pub max_iterations_high: u32,
}

impl Default for TaskLimitsConfig {
    fn default() -> Self {
        Self {
            max_iterations_low: default_max_iterations_low(),
            max_iterations_medium: default_max_iterations_medium(),
            max_iterations_high: default_max_iterations_high(),
        }
    }
}

fn default_health_port() -> u16 {
    9091
}

fn default_state_db_path() -> String {
    "data/kernel_state.db".to_string()
}

fn default_max_iterations_low() -> u32 {
    50
}

fn default_max_iterations_medium() -> u32 {
    200
}

fn default_max_iterations_high() -> u32 {
    1000
}

/// Configuration for tasks running in autonomous mode.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AutonomousModeConfig {
    /// Maximum iterations before the agent loop terminates.
    /// Default: 10000 — high enough to be effectively unlimited for any
    /// real-world long-running workflow.
    #[serde(default = "default_autonomous_max_iterations")]
    pub max_iterations: u32,
    /// Wall-clock timeout for the entire task, in seconds.
    /// Default: 86400 (24 hours).
    #[serde(default = "default_autonomous_task_timeout_secs")]
    pub task_timeout_secs: u64,
    /// Per-tool timeout for in-process tool calls, in seconds.
    /// Default: 600 (10 minutes) — covers long-running tools like compilers,
    /// test runners, and data-processing pipelines.
    #[serde(default = "default_autonomous_tool_timeout_seconds")]
    pub tool_timeout_seconds: u64,
    /// Maximum parallel tool calls per turn for autonomous tasks.
    /// Default: 10.
    #[serde(default = "default_autonomous_max_parallel")]
    pub max_parallel_tool_calls: usize,
}

impl Default for AutonomousModeConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_autonomous_max_iterations(),
            task_timeout_secs: default_autonomous_task_timeout_secs(),
            tool_timeout_seconds: default_autonomous_tool_timeout_seconds(),
            max_parallel_tool_calls: default_autonomous_max_parallel(),
        }
    }
}

fn default_autonomous_max_iterations() -> u32 {
    10_000
}

fn default_autonomous_task_timeout_secs() -> u64 {
    86_400 // 24 hours
}

fn default_autonomous_tool_timeout_seconds() -> u64 {
    600 // 10 minutes
}

fn default_autonomous_max_parallel() -> usize {
    10
}

fn default_per_agent_rate_limit() -> u32 {
    100
}

fn default_max_concurrent_sandbox_children() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(2)
}

fn default_allow_parallel_tool_calls() -> bool {
    true
}

fn default_max_parallel_tool_calls() -> usize {
    5
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
    /// Number of recent entries to verify during startup chain integrity check.
    /// `0` verifies the full chain (may be slow for large logs).
    /// Default: 1000.
    #[serde(default = "default_verify_last_n_entries")]
    pub verify_last_n_entries: u64,
}

fn default_verify_last_n_entries() -> u64 {
    1000
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct WorkspaceConfig {
    /// Additional directories the agent can access beyond `data_dir`.
    /// Each must be an absolute path. System directories (/, /etc, /var, /root, /home)
    /// are rejected at config load time.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
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
    /// Configurable workspace directories the agent can access beyond `data_dir`.
    #[serde(default)]
    pub workspace: WorkspaceConfig,
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
    /// HTTP request timeout for Ollama inference calls, in seconds.
    /// Cloud-proxied and large models may need 300–600s. Default: 300.
    #[serde(default = "default_ollama_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

fn default_ollama_request_timeout_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// Maximum output tokens for Anthropic (and other providers that accept a `max_tokens` param).
    /// Defaults to 8192. Set higher for long-form generation tasks.
    #[serde(default = "default_llm_max_tokens")]
    pub max_tokens: u32,
    /// Context window size passed to Ollama as `num_ctx`.
    /// Defaults to 32768. Increase for models with larger context support (e.g. 131072).
    #[serde(default = "default_ollama_context_window")]
    pub ollama_context_window: u32,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            custom_base_url: None,
            openai_base_url: None,
            anthropic_base_url: None,
            gemini_base_url: None,
            max_tokens: default_llm_max_tokens(),
            ollama_context_window: default_ollama_context_window(),
        }
    }
}

fn default_llm_max_tokens() -> u32 {
    8192
}

fn default_ollama_context_window() -> u32 {
    32768
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

/// File-based logging configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoggingSettings {
    /// Directory for rolling log files. Empty string disables file logging.
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
    /// Minimum log level: trace | debug | info | warn | error
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Output format: "text" (human-readable) or "json" (structured, for log aggregators).
    #[serde(default = "default_log_format")]
    pub log_format: String,
}

fn default_log_dir() -> String {
    "/tmp/agentos/logs".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "text".to_string()
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            log_dir: default_log_dir(),
            log_level: default_log_level(),
            log_format: default_log_format(),
        }
    }
}

/// Load kernel configuration from a TOML file.
pub fn load_config(path: &std::path::Path) -> Result<KernelConfig, anyhow::Error> {
    let content = std::fs::read_to_string(path)?;
    let mut config: KernelConfig = toml::from_str(&content)?;
    apply_env_overrides(&mut config);
    validate_task_limits(&config.kernel.task_limits)?;
    validate_event_channel(&config.kernel.events)?;
    validate_llm_settings(&config.llm)?;
    validate_workspace_paths(&config.tools.workspace)?;
    validate_logging_settings(&config.logging)?;
    validate_sandbox_settings(&config.kernel)?;
    config
        .context_budget
        .validate()
        .map_err(|e| anyhow::anyhow!("context_budget: {}", e))?;
    warn_on_tmp_paths(&config);
    Ok(config)
}

/// Validate that workspace paths are absolute and not forbidden system directories.
fn validate_workspace_paths(workspace: &WorkspaceConfig) -> Result<(), anyhow::Error> {
    // Exact paths that are too broad to be safe workspace roots.
    const FORBIDDEN: &[&str] = &[
        "/", "/etc", "/var", "/root", "/home", "/proc", "/sys", "/dev", "/boot", "/usr",
    ];

    for path_str in &workspace.allowed_paths {
        let p = std::path::Path::new(path_str);
        if !p.is_absolute() {
            anyhow::bail!(
                "tools.workspace.allowed_paths: '{}' is not an absolute path; \
                 workspace paths must start with '/'",
                path_str
            );
        }
        if FORBIDDEN.contains(&path_str.as_str()) {
            anyhow::bail!(
                "tools.workspace.allowed_paths: '{}' is a system directory and \
                 cannot be used as a workspace root",
                path_str
            );
        }
        // Must have at least one path component beyond the filesystem root.
        let components: Vec<_> = p.components().collect();
        if components.len() < 2 {
            anyhow::bail!(
                "tools.workspace.allowed_paths: '{}' is too broad — \
                 must include at least one subdirectory (e.g. /home/user/project)",
                path_str
            );
        }
    }
    Ok(())
}

fn validate_llm_settings(settings: &LlmSettings) -> Result<(), anyhow::Error> {
    if settings.max_tokens == 0 {
        anyhow::bail!(
            "llm.max_tokens must be > 0 (got 0); \
             set a positive value such as 8192"
        );
    }
    if settings.ollama_context_window == 0 {
        anyhow::bail!(
            "llm.ollama_context_window must be > 0 (got 0); \
             set a positive value such as 32768"
        );
    }
    Ok(())
}

fn validate_task_limits(limits: &TaskLimitsConfig) -> Result<(), anyhow::Error> {
    if limits.max_iterations_high == 0 {
        anyhow::bail!(
            "task_limits.max_iterations_high must be > 0 (got 0); \
             agents need at least one iteration to make progress"
        );
    }
    if limits.max_iterations_low > limits.max_iterations_medium
        || limits.max_iterations_medium > limits.max_iterations_high
    {
        anyhow::bail!(
            "task_limits must satisfy low <= medium <= high, got: low={}, medium={}, high={}",
            limits.max_iterations_low,
            limits.max_iterations_medium,
            limits.max_iterations_high,
        );
    }
    Ok(())
}

fn validate_event_channel(cfg: &EventChannelConfig) -> Result<(), anyhow::Error> {
    if cfg.channel_capacity == 0 {
        anyhow::bail!(
            "kernel.events.channel_capacity must be > 0 (got 0); \
             tokio mpsc channels require at least one buffer slot"
        );
    }
    Ok(())
}

fn validate_logging_settings(logging: &LoggingSettings) -> Result<(), anyhow::Error> {
    if !["text", "json"].contains(&logging.log_format.as_str()) {
        anyhow::bail!(
            "logging.log_format must be \"text\" or \"json\", got \"{}\"",
            logging.log_format
        );
    }
    Ok(())
}

fn validate_sandbox_settings(kernel: &KernelSettings) -> Result<(), anyhow::Error> {
    if kernel.max_concurrent_sandbox_children == 0 {
        anyhow::bail!(
            "kernel.max_concurrent_sandbox_children must be > 0 (got 0); \
             at least one sandbox child slot is required"
        );
    }
    if kernel.sandbox_policy == SandboxPolicy::Never {
        tracing::warn!(
            "kernel.sandbox_policy is set to 'never' — all tools run unsandboxed. \
             This is NOT safe for production. Use 'trust_aware' or 'always' instead."
        );
    }
    Ok(())
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

    if let Ok(path) = std::env::var("AGENTOS_STATE_DB_PATH") {
        if !path.trim().is_empty() {
            config.kernel.state_db_path = path;
        }
    }

    if let Ok(val) = std::env::var("AGENTOS_LLM_MAX_TOKENS") {
        if let Ok(n) = val.trim().parse::<u32>() {
            config.llm.max_tokens = n;
        }
    }

    if let Ok(val) = std::env::var("AGENTOS_OLLAMA_CONTEXT_WINDOW") {
        if let Ok(n) = val.trim().parse::<u32>() {
            config.llm.ollama_context_window = n;
        }
    }
}

/// Tracks which (config_key, path) pairs have already been warned about
/// so that repeated `load_config()` calls within the same process don't
/// flood the log with identical warnings.
static WARNED_TMP_PATHS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn warned_paths() -> &'static Mutex<HashSet<String>> {
    WARNED_TMP_PATHS.get_or_init(|| Mutex::new(HashSet::new()))
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

    let warned = warned_paths();

    for (name, path) in runtime_paths {
        if is_tmp_path(path) {
            let key = format!("{}:{}", name, path);
            let already_warned = warned.lock().unwrap().contains(&key);
            if !already_warned {
                tracing::warn!(
                    config_key = %name,
                    path = %path,
                    "Runtime path points to a temporary location; use persistent storage in production"
                );
                warned.lock().unwrap().insert(key);
            }
        }
    }

    // Only warn for model_cache_dir when it is absolute; relative paths inherit
    // their safety from tools.data_dir, which is already checked above.
    let model_cache = config.memory.model_cache_dir.as_str();
    if std::path::Path::new(model_cache).is_absolute() && is_tmp_path(model_cache) {
        let key = format!("memory.model_cache_dir:{}", model_cache);
        let already_warned = warned.lock().unwrap().contains(&key);
        if !already_warned {
            tracing::warn!(
                config_key = "memory.model_cache_dir",
                path = %model_cache,
                "Runtime path points to a temporary location; use persistent storage in production"
            );
            warned.lock().unwrap().insert(key);
        }
    }

    let state_db_path = config.kernel.state_db_path.as_str();
    if std::path::Path::new(state_db_path).is_absolute() && is_tmp_path(state_db_path) {
        let key = format!("kernel.state_db_path:{}", state_db_path);
        let already_warned = warned.lock().unwrap().contains(&key);
        if !already_warned {
            tracing::warn!(
                config_key = "kernel.state_db_path",
                path = %state_db_path,
                "Runtime path points to a temporary location; use persistent storage in production"
            );
            warned.lock().unwrap().insert(key);
        }
    }
}

fn is_tmp_path(path: &str) -> bool {
    let p = std::path::Path::new(path);
    p.starts_with("/tmp") || p.starts_with("/var/tmp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_limits_default_when_omitted_from_toml() {
        let config: KernelConfig = toml::from_str(
            r#"
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 60
context_window_max_entries = 100
context_window_token_budget = 8000

[secrets]
vault_path = "/tmp/agentos/vault/secrets.db"

[audit]
log_path = "/tmp/agentos/data/audit.db"

[tools]
core_tools_dir = "/tmp/agentos/tools/core"
user_tools_dir = "/tmp/agentos/tools/user"
data_dir = "/tmp/agentos/data"

[bus]
socket_path = "/tmp/agentos/agentos.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"
"#,
        )
        .expect("config should parse");

        assert_eq!(config.kernel.task_limits.max_iterations_low, 50);
        assert_eq!(config.kernel.task_limits.max_iterations_medium, 200);
        assert_eq!(config.kernel.task_limits.max_iterations_high, 1000);
        assert_eq!(config.kernel.state_db_path, "data/kernel_state.db");
    }

    #[test]
    fn task_limits_rejects_inverted_ordering() {
        let toml_str = r#"
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 60
context_window_max_entries = 100
context_window_token_budget = 8000

[kernel.task_limits]
max_iterations_low = 50
max_iterations_medium = 10
max_iterations_high = 5

[secrets]
vault_path = "/tmp/agentos/vault/secrets.db"

[audit]
log_path = "/tmp/agentos/data/audit.db"

[tools]
core_tools_dir = "/tmp/agentos/tools/core"
user_tools_dir = "/tmp/agentos/tools/user"
data_dir = "/tmp/agentos/data"

[bus]
socket_path = "/tmp/agentos/agentos.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"
"#;
        // Write to a temp file so we can use load_config
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, toml_str).unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(
            err.to_string().contains("low <= medium <= high"),
            "expected ordering error, got: {err}"
        );
    }

    #[test]
    fn task_limits_parse_from_nested_kernel_table() {
        let config: KernelConfig = toml::from_str(
            r#"
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 60
context_window_max_entries = 100
context_window_token_budget = 8000

[kernel.task_limits]
max_iterations_low = 7
max_iterations_medium = 19
max_iterations_high = 41

[secrets]
vault_path = "/tmp/agentos/vault/secrets.db"

[audit]
log_path = "/tmp/agentos/data/audit.db"

[tools]
core_tools_dir = "/tmp/agentos/tools/core"
user_tools_dir = "/tmp/agentos/tools/user"
data_dir = "/tmp/agentos/data"

[bus]
socket_path = "/tmp/agentos/agentos.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"
"#,
        )
        .expect("config should parse");

        assert_eq!(config.kernel.task_limits.max_iterations_low, 7);
        assert_eq!(config.kernel.task_limits.max_iterations_medium, 19);
        assert_eq!(config.kernel.task_limits.max_iterations_high, 41);
    }

    const MINIMAL_TOML: &str = r#"
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 60
context_window_max_entries = 100
context_window_token_budget = 8000

[secrets]
vault_path = "/tmp/agentos/vault/secrets.db"

[audit]
log_path = "/tmp/agentos/data/audit.db"

[tools]
core_tools_dir = "/tmp/agentos/tools/core"
user_tools_dir = "/tmp/agentos/tools/user"
data_dir = "/tmp/agentos/data"

[bus]
socket_path = "/tmp/agentos/agentos.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"
"#;

    #[test]
    fn llm_settings_defaults_when_section_omitted() {
        let config: KernelConfig = toml::from_str(MINIMAL_TOML).expect("config should parse");
        assert_eq!(config.llm.max_tokens, 8192);
        assert_eq!(config.llm.ollama_context_window, 32768);
    }

    #[test]
    fn llm_settings_parses_explicit_values() {
        let toml_str = format!(
            "{}\n[llm]\nmax_tokens = 16384\nollama_context_window = 131072\n",
            MINIMAL_TOML
        );
        let config: KernelConfig = toml::from_str(&toml_str).expect("config should parse");
        assert_eq!(config.llm.max_tokens, 16384);
        assert_eq!(config.llm.ollama_context_window, 131072);
    }

    #[test]
    fn llm_settings_rejects_zero_max_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        let toml_str = format!("{}\n[llm]\nmax_tokens = 0\n", MINIMAL_TOML);
        std::fs::write(&path, toml_str).unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(
            err.to_string().contains("llm.max_tokens must be > 0"),
            "expected max_tokens error, got: {err}"
        );
    }

    #[test]
    fn llm_settings_rejects_zero_ollama_context_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        let toml_str = format!("{}\n[llm]\nollama_context_window = 0\n", MINIMAL_TOML);
        std::fs::write(&path, toml_str).unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(
            err.to_string()
                .contains("llm.ollama_context_window must be > 0"),
            "expected context_window error, got: {err}"
        );
    }

    #[test]
    fn sandbox_policy_defaults_to_trust_aware() {
        let config: KernelConfig = toml::from_str(MINIMAL_TOML).expect("should parse");
        assert_eq!(config.kernel.sandbox_policy, SandboxPolicy::TrustAware);
    }

    #[test]
    fn sandbox_policy_parses_always() {
        let toml_str = MINIMAL_TOML.replace(
            "context_window_token_budget = 8000",
            "context_window_token_budget = 8000\nsandbox_policy = \"always\"",
        );
        let config: KernelConfig = toml::from_str(&toml_str).expect("should parse");
        assert_eq!(config.kernel.sandbox_policy, SandboxPolicy::Always);
    }

    #[test]
    fn sandbox_policy_parses_never() {
        let toml_str = MINIMAL_TOML.replace(
            "context_window_token_budget = 8000",
            "context_window_token_budget = 8000\nsandbox_policy = \"never\"",
        );
        let config: KernelConfig = toml::from_str(&toml_str).expect("should parse");
        assert_eq!(config.kernel.sandbox_policy, SandboxPolicy::Never);
    }

    #[test]
    fn max_concurrent_sandbox_children_defaults_nonzero() {
        let config: KernelConfig = toml::from_str(MINIMAL_TOML).expect("should parse");
        assert!(config.kernel.max_concurrent_sandbox_children >= 2);
    }

    #[test]
    fn max_concurrent_sandbox_children_rejects_zero() {
        let toml_str = MINIMAL_TOML.replace(
            "context_window_token_budget = 8000",
            "context_window_token_budget = 8000\nmax_concurrent_sandbox_children = 0",
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, toml_str).unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(
            err.to_string().contains("must be > 0"),
            "expected concurrency error, got: {err}"
        );
    }
}
