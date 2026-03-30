use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
    pub context: ContextConfig,
    #[serde(default)]
    pub health_monitor: HealthMonitorConfig,
    #[serde(default)]
    pub preflight: PreflightConfig,
    #[serde(default)]
    pub logging: LoggingSettings,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    /// MCP (Model Context Protocol) adapter configuration.
    /// Defines external MCP server processes to connect at kernel boot.
    #[serde(default)]
    pub mcp: McpConfig,
    /// Tool registry configuration for marketplace install/publish/search.
    #[serde(default)]
    pub registry: RegistryConfig,
    /// Agent scratchpad configuration (graph-aware knowledge store).
    #[serde(default)]
    pub scratchpad: ScratchpadConfig,
    /// OpenTelemetry export configuration.
    #[serde(default)]
    pub otel: OtelConfig,
}

/// Configuration for the Unified Notification and Interaction System (UNIS).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NotificationsConfig {
    /// Maximum messages stored in the user inbox (oldest read messages purged on overflow).
    #[serde(default = "default_max_inbox_size")]
    pub max_inbox_size: usize,
    /// Send a notification to the user inbox when a root task completes successfully.
    #[serde(default = "default_true")]
    pub notify_on_task_complete: bool,
    /// Send a notification to the user inbox when a root task fails.
    #[serde(default = "default_true")]
    pub notify_on_task_failed: bool,
    /// Pluggable delivery adapter configuration.
    #[serde(default)]
    pub adapters: NotificationAdaptersConfig,
}

fn default_max_inbox_size() -> usize {
    1000
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            max_inbox_size: default_max_inbox_size(),
            notify_on_task_complete: true,
            notify_on_task_failed: true,
            adapters: NotificationAdaptersConfig::default(),
        }
    }
}

/// Configuration for all pluggable delivery adapters.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct NotificationAdaptersConfig {
    #[serde(default)]
    pub webhook: WebhookAdapterConfig,
    #[serde(default)]
    pub desktop: DesktopAdapterConfig,
    #[serde(default)]
    pub slack: SlackAdapterConfig,
}

/// Outbound HTTPS webhook adapter configuration.
///
/// Custom `Debug` redacts the `secret` field to prevent credential exposure in logs.
#[derive(Clone, Deserialize, Serialize)]
pub struct WebhookAdapterConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    /// HMAC-SHA256 secret for X-AgentOS-Signature header. Empty = no signature.
    #[serde(default)]
    pub secret: String,
    /// Minimum priority to deliver (info/warning/urgent/critical). Default: "warning".
    #[serde(default = "default_warning_priority")]
    pub min_priority: String,
    /// Maximum delivery retry attempts. Default: 3.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Seconds to wait between retries. Default: 5.
    #[serde(default = "default_retry_delay_secs")]
    pub retry_delay_secs: u64,
    /// Per-request timeout in seconds. Default: 10.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for WebhookAdapterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            secret: String::new(),
            min_priority: default_warning_priority(),
            max_retries: default_max_retries(),
            retry_delay_secs: default_retry_delay_secs(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl std::fmt::Debug for WebhookAdapterConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookAdapterConfig")
            .field("enabled", &self.enabled)
            .field("url", &self.url)
            .field(
                "secret",
                &if self.secret.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .field("min_priority", &self.min_priority)
            .field("max_retries", &self.max_retries)
            .field("retry_delay_secs", &self.retry_delay_secs)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

/// Desktop notification adapter configuration (Linux only).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DesktopAdapterConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum priority to show as desktop notification. Default: "warning".
    #[serde(default = "default_warning_priority")]
    pub min_priority: String,
    /// Show task completion notifications even if they are at info priority.
    #[serde(default = "default_true")]
    pub notify_on_task_complete: bool,
}

impl Default for DesktopAdapterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_priority: default_warning_priority(),
            notify_on_task_complete: true,
        }
    }
}

/// Slack incoming-webhook adapter configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SlackAdapterConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub webhook_url: String,
    /// Minimum priority to send to Slack. Default: "warning".
    #[serde(default = "default_warning_priority")]
    pub min_priority: String,
    /// Include full message body (true) or subject only (false). Default: true.
    #[serde(default = "default_true")]
    pub include_body: bool,
    /// Maximum delivery retry attempts. Default: 3.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Seconds to wait before first retry (doubles each attempt). Default: 2.
    #[serde(default = "default_slack_retry_delay_secs")]
    pub retry_delay_secs: u64,
}

impl Default for SlackAdapterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            webhook_url: String::new(),
            min_priority: default_warning_priority(),
            include_body: true,
            max_retries: default_max_retries(),
            retry_delay_secs: default_slack_retry_delay_secs(),
        }
    }
}

fn default_slack_retry_delay_secs() -> u64 {
    2
}

fn default_warning_priority() -> String {
    "warning".to_string()
}

fn default_max_retries() -> u32 {
    3
}

fn default_retry_delay_secs() -> u64 {
    5
}

fn default_timeout_secs() -> u64 {
    10
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
    #[serde(default)]
    pub context: ContextMemoryConfig,
}

/// Per-agent context memory configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextMemoryConfig {
    /// Enable context memory injection.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum token budget per agent's context memory document.
    #[serde(default = "default_context_memory_max_tokens")]
    pub max_tokens: usize,
    /// Maximum versions retained in history per agent.
    #[serde(default = "default_context_memory_max_versions")]
    pub max_versions: usize,
    /// Database file name (relative to data_dir).
    #[serde(default = "default_context_memory_db_path")]
    pub db_path: String,
}

fn default_context_memory_max_tokens() -> usize {
    4096
}

fn default_context_memory_max_versions() -> usize {
    50
}

fn default_context_memory_db_path() -> String {
    "context_memory.db".to_string()
}

impl Default for ContextMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tokens: default_context_memory_max_tokens(),
            max_versions: default_context_memory_max_versions(),
            db_path: default_context_memory_db_path(),
        }
    }
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            model_cache_dir: default_model_cache_dir(),
            extraction: crate::memory_extraction::ExtractionConfig::default(),
            consolidation: crate::consolidation::ConsolidationConfig::default(),
            context: ContextMemoryConfig::default(),
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OtelProtocol {
    Grpc,
    Http,
}

#[allow(clippy::derivable_impls)]
impl Default for OtelProtocol {
    fn default() -> Self {
        Self::Grpc
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OtelConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
    #[serde(default = "default_otel_endpoint")]
    pub endpoint: String,
    #[serde(default)]
    pub protocol: OtelProtocol,
    #[serde(default = "default_otel_service_name")]
    pub service_name: String,
    #[serde(default = "default_otel_sample_rate")]
    pub sample_rate: f64,
    #[serde(default = "default_true")]
    pub scrub_tool_inputs: bool,
    #[serde(default = "default_true")]
    pub scrub_tool_outputs: bool,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: default_false(),
            endpoint: default_otel_endpoint(),
            protocol: OtelProtocol::default(),
            service_name: default_otel_service_name(),
            sample_rate: default_otel_sample_rate(),
            scrub_tool_inputs: default_true(),
            scrub_tool_outputs: default_true(),
        }
    }
}

fn default_otel_endpoint() -> String {
    "http://localhost:4317".to_string()
}

fn default_otel_service_name() -> String {
    "agentos".to_string()
}

fn default_otel_sample_rate() -> f64 {
    1.0
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

// ── MCP configuration ────────────────────────────────────────────────────────

/// Configuration for the MCP (Model Context Protocol) adapter layer.
///
/// Lists external MCP server processes to connect at kernel boot. Each server
/// is spawned as a child process connected via stdio JSON-RPC. Its tools are
/// registered in the kernel `ToolRunner` with `TrustTier::Community`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct McpConfig {
    /// MCP server processes to connect at kernel boot.
    /// Each entry spawns a child process via stdio JSON-RPC.
    ///
    /// Example in `config/default.toml`:
    /// ```toml
    /// [[mcp.servers]]
    /// name = "filesystem"
    /// command = "npx"
    /// args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    /// ```
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

/// Configuration for a single external MCP server process.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    /// Human-readable name for this server (used in log messages).
    pub name: String,
    /// Path or name of the executable to spawn (e.g. `"npx"`, `"python3"`).
    pub command: String,
    /// Arguments passed to `command` (e.g. `["-y", "@modelcontextprotocol/server-filesystem"]`).
    #[serde(default)]
    pub args: Vec<String>,
}

/// Agent scratchpad configuration for the graph-aware knowledge store.
///
/// Controls BFS graph traversal depth and budget limits for automatic
/// injection of related scratchpad notes into the LLM context window.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScratchpadConfig {
    /// Whether scratchpad context injection is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Database path (relative to `tools.data_dir` or absolute).
    #[serde(default = "default_scratchpad_db_path")]
    pub db_path: String,
    /// BFS traversal depth for context injection (0 = seed page only).
    #[serde(default = "default_scratchpad_context_depth")]
    pub context_depth: usize,
    /// Maximum pages injected per inference call.
    #[serde(default = "default_scratchpad_max_context_pages")]
    pub max_context_pages: usize,
    /// Maximum total bytes of scratchpad content injected per inference call.
    #[serde(default = "default_scratchpad_max_context_bytes")]
    pub max_context_bytes: usize,
    /// Maximum content size per individual page (bytes).
    #[serde(default = "default_scratchpad_max_page_size")]
    pub max_page_size: usize,
    /// Maximum pages per agent.
    #[serde(default = "default_scratchpad_max_pages_per_agent")]
    pub max_pages_per_agent: usize,
    /// Automatically generate a scratchpad note when a task completes.
    #[serde(default = "default_true")]
    pub auto_write_on_completion: bool,
    /// Minimum episodic entries for a task to qualify for auto-write (skip trivial tasks).
    #[serde(default = "default_scratchpad_auto_write_min_steps")]
    pub auto_write_min_steps: usize,
    /// Maximum bytes for an auto-generated scratchpad note.
    #[serde(default = "default_scratchpad_auto_write_max_summary")]
    pub auto_write_max_summary: usize,
}

impl Default for ScratchpadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: default_scratchpad_db_path(),
            context_depth: default_scratchpad_context_depth(),
            max_context_pages: default_scratchpad_max_context_pages(),
            max_context_bytes: default_scratchpad_max_context_bytes(),
            max_page_size: default_scratchpad_max_page_size(),
            max_pages_per_agent: default_scratchpad_max_pages_per_agent(),
            auto_write_on_completion: true,
            auto_write_min_steps: default_scratchpad_auto_write_min_steps(),
            auto_write_max_summary: default_scratchpad_auto_write_max_summary(),
        }
    }
}

fn default_scratchpad_db_path() -> String {
    "scratchpad.db".to_string()
}

fn default_scratchpad_context_depth() -> usize {
    2
}

fn default_scratchpad_max_context_pages() -> usize {
    5
}

fn default_scratchpad_max_context_bytes() -> usize {
    8192
}

fn default_scratchpad_max_page_size() -> usize {
    65536 // 64 KB
}

fn default_scratchpad_max_pages_per_agent() -> usize {
    1000
}

fn default_scratchpad_auto_write_min_steps() -> usize {
    3
}

fn default_scratchpad_auto_write_max_summary() -> usize {
    2048
}

/// Context window management configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextConfig {
    /// Summarization strategy when context budget compression triggers.
    /// - `llm`: Use the agent's LLM adapter for real summarization (falls back to concat on error)
    /// - `concat`: Concatenate entry snippets (legacy behavior)
    /// - `off`: No summary entry created; entries are silently evicted
    #[serde(default = "default_summarization_mode")]
    pub summarization_mode: SummarizationMode,
    /// Maximum characters of entry text sent to the summarizer LLM per compression event.
    #[serde(default = "default_summarization_max_input_chars")]
    pub summarization_max_input_chars: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            summarization_mode: SummarizationMode::default(),
            summarization_max_input_chars: default_summarization_max_input_chars(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SummarizationMode {
    /// LLM-generated summaries (best-effort, falls back to concat).
    #[default]
    Llm,
    /// Concatenate entry snippets (legacy behavior).
    Concat,
    /// No summary — entries are silently evicted.
    Off,
}

fn default_summarization_mode() -> SummarizationMode {
    SummarizationMode::Llm
}

fn default_summarization_max_input_chars() -> usize {
    8000
}

/// Tool registry (marketplace) configuration.
///
/// Controls where `agentctl tool search/add/publish` connect to fetch and
/// publish community tools.  Defaults to the public AgentOS registry.
/// Override with the `AGENTOS_REGISTRY` environment variable for local or
/// self-hosted registries.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryConfig {
    /// Base URL of the tool registry HTTP API.
    #[serde(default = "default_registry_url")]
    pub url: String,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            url: default_registry_url(),
        }
    }
}

fn default_registry_url() -> String {
    "https://registry.agentos.dev".to_string()
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
    validate_otel_config(&config.otel)?;
    validate_sandbox_settings(&config.kernel)?;
    validate_notification_adapters(&config.notifications.adapters)?;
    validate_mcp_config(&config.mcp)?;
    config
        .context_budget
        .validate()
        .map_err(|e| anyhow::anyhow!("context_budget: {}", e))?;
    warn_on_tmp_paths(&config);
    Ok(config)
}

/// Validate that all MCP server entries have non-empty name and command fields.
fn validate_mcp_config(mcp: &McpConfig) -> Result<(), anyhow::Error> {
    for (i, srv) in mcp.servers.iter().enumerate() {
        if srv.name.trim().is_empty() {
            anyhow::bail!("mcp.servers[{}]: 'name' must not be empty", i);
        }
        if srv.command.trim().is_empty() {
            anyhow::bail!(
                "mcp.servers[{}] ({}): 'command' must not be empty",
                i,
                srv.name
            );
        }
    }
    Ok(())
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

fn validate_otel_config(otel: &OtelConfig) -> Result<(), anyhow::Error> {
    if !(0.0..=1.0).contains(&otel.sample_rate) {
        anyhow::bail!(
            "otel.sample_rate must be between 0.0 and 1.0 inclusive, got {}",
            otel.sample_rate
        );
    }
    if otel.enabled && otel.endpoint.trim().is_empty() {
        anyhow::bail!("otel.enabled is true but otel.endpoint is empty");
    }
    if otel.service_name.trim().is_empty() {
        anyhow::bail!("otel.service_name must not be empty");
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
    // Tokio Semaphore panics above MAX_PERMITS (usize::MAX >> 3); cap at a sane limit.
    if kernel.max_concurrent_sandbox_children > 1024 {
        tracing::warn!(
            value = kernel.max_concurrent_sandbox_children,
            "kernel.max_concurrent_sandbox_children is unusually high; \
             values above 1024 may exhaust system resources"
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

fn validate_notification_adapters(
    adapters: &NotificationAdaptersConfig,
) -> Result<(), anyhow::Error> {
    if adapters.webhook.enabled {
        if adapters.webhook.url.is_empty() {
            anyhow::bail!(
                "notifications.adapters.webhook.enabled is true but url is empty; \
                 set a valid HTTPS webhook URL"
            );
        }
        if adapters.webhook.max_retries > 10 {
            anyhow::bail!(
                "notifications.adapters.webhook.max_retries is {} (max 10)",
                adapters.webhook.max_retries
            );
        }
    }
    if adapters.slack.enabled {
        if adapters.slack.webhook_url.is_empty() {
            anyhow::bail!(
                "notifications.adapters.slack.enabled is true but webhook_url is empty; \
                 set a valid Slack incoming-webhook URL"
            );
        }
        if adapters.slack.max_retries > 10 {
            anyhow::bail!(
                "notifications.adapters.slack.max_retries is {} (max 10)",
                adapters.slack.max_retries
            );
        }
    }
    Ok(())
}

fn apply_env_overrides(config: &mut KernelConfig) {
    apply_env_overrides_from(config, |key| std::env::var(key).ok());
}

fn apply_env_overrides_from<F>(config: &mut KernelConfig, lookup: F)
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(new_data_dir) = nonempty_env(&lookup, "AGENTOS_DATA_DIR") {
        let old_data_dir = config.tools.data_dir.clone();
        config.tools.data_dir = new_data_dir.clone();
        rebase_data_dir_paths(config, &old_data_dir, &new_data_dir);
    }

    apply_string_override(
        &lookup,
        "AGENTOS_CORE_TOOLS_DIR",
        &mut config.tools.core_tools_dir,
    );
    apply_string_override(
        &lookup,
        "AGENTOS_USER_TOOLS_DIR",
        &mut config.tools.user_tools_dir,
    );
    apply_string_override(
        &lookup,
        "AGENTOS_AUDIT_LOG_PATH",
        &mut config.audit.log_path,
    );
    apply_string_override(
        &lookup,
        "AGENTOS_VAULT_PATH",
        &mut config.secrets.vault_path,
    );
    apply_string_override(
        &lookup,
        "AGENTOS_BUS_SOCKET_PATH",
        &mut config.bus.socket_path,
    );
    apply_string_override(
        &lookup,
        "AGENTOS_STATE_DB_PATH",
        &mut config.kernel.state_db_path,
    );
    apply_string_override(
        &lookup,
        "AGENTOS_MODEL_CACHE_DIR",
        &mut config.memory.model_cache_dir,
    );
    apply_string_override(&lookup, "AGENTOS_OLLAMA_HOST", &mut config.ollama.host);
    apply_string_override(
        &lookup,
        "AGENTOS_OLLAMA_MODEL",
        &mut config.ollama.default_model,
    );
    apply_parsed_override(
        &lookup,
        "AGENTOS_OLLAMA_REQUEST_TIMEOUT_SECS",
        &mut config.ollama.request_timeout_secs,
    );
    apply_parsed_override(
        &lookup,
        "AGENTOS_HEALTH_PORT",
        &mut config.kernel.health_port,
    );

    if let Some(url) = nonempty_env(&lookup, "AGENTOS_LLM_URL") {
        config.llm.custom_base_url = Some(url);
    }
    if let Some(url) = nonempty_env(&lookup, "AGENTOS_OPENAI_BASE_URL") {
        config.llm.openai_base_url = Some(url);
    }
    if let Some(url) = nonempty_env(&lookup, "AGENTOS_LLM_ANTHROPIC_BASE_URL") {
        config.llm.anthropic_base_url = Some(url);
    }
    if let Some(url) = nonempty_env(&lookup, "AGENTOS_LLM_GEMINI_BASE_URL") {
        config.llm.gemini_base_url = Some(url);
    }

    apply_bool_override(&lookup, "AGENTOS_OTEL_ENABLED", &mut config.otel.enabled);
    apply_string_override(&lookup, "AGENTOS_OTEL_ENDPOINT", &mut config.otel.endpoint);
    apply_string_override(
        &lookup,
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        &mut config.otel.endpoint,
    );
    apply_string_override(
        &lookup,
        "AGENTOS_OTEL_SERVICE_NAME",
        &mut config.otel.service_name,
    );
    apply_string_override(&lookup, "OTEL_SERVICE_NAME", &mut config.otel.service_name);
    apply_parsed_override(
        &lookup,
        "AGENTOS_OTEL_SAMPLE_RATE",
        &mut config.otel.sample_rate,
    );

    apply_parsed_override(
        &lookup,
        "AGENTOS_LLM_MAX_TOKENS",
        &mut config.llm.max_tokens,
    );
    apply_parsed_override(
        &lookup,
        "AGENTOS_OLLAMA_CONTEXT_WINDOW",
        &mut config.llm.ollama_context_window,
    );

    apply_string_override(&lookup, "AGENTOS_REGISTRY", &mut config.registry.url);
}

fn nonempty_env<F>(lookup: &F, key: &str) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    lookup(key).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn apply_string_override<F>(lookup: &F, key: &str, target: &mut String)
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(value) = nonempty_env(lookup, key) {
        *target = value;
    }
}

fn apply_bool_override<F>(lookup: &F, key: &str, target: &mut bool)
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(value) = nonempty_env(lookup, key) {
        if let Ok(parsed) = value.parse::<bool>() {
            *target = parsed;
        }
    }
}

fn apply_parsed_override<F, T>(lookup: &F, key: &str, target: &mut T)
where
    F: Fn(&str) -> Option<String>,
    T: std::str::FromStr,
{
    if let Some(value) = nonempty_env(lookup, key) {
        if let Ok(parsed) = value.parse::<T>() {
            *target = parsed;
        }
    }
}

fn rebase_data_dir_paths(config: &mut KernelConfig, old_data_dir: &str, new_data_dir: &str) {
    rebase_runtime_path(&mut config.kernel.state_db_path, old_data_dir, new_data_dir);
    rebase_runtime_path(&mut config.audit.log_path, old_data_dir, new_data_dir);
    rebase_runtime_path(&mut config.secrets.vault_path, old_data_dir, new_data_dir);
    rebase_runtime_path(&mut config.bus.socket_path, old_data_dir, new_data_dir);
    rebase_runtime_path(&mut config.tools.core_tools_dir, old_data_dir, new_data_dir);
    rebase_runtime_path(&mut config.tools.user_tools_dir, old_data_dir, new_data_dir);
    rebase_runtime_path(
        &mut config.memory.model_cache_dir,
        old_data_dir,
        new_data_dir,
    );
}

fn rebase_runtime_path(path: &mut String, old_root: &str, new_root: &str) {
    let old_root = Path::new(old_root);
    let current = Path::new(path);
    if let Ok(relative) = current.strip_prefix(old_root) {
        *path = PathBuf::from(new_root)
            .join(relative)
            .to_string_lossy()
            .into_owned();
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
    fn otel_defaults_when_section_omitted() {
        let config: KernelConfig = toml::from_str(MINIMAL_TOML).expect("config should parse");
        assert!(!config.otel.enabled);
        assert_eq!(config.otel.endpoint, "http://localhost:4317");
        assert_eq!(config.otel.protocol, OtelProtocol::Grpc);
        assert_eq!(config.otel.service_name, "agentos");
        assert_eq!(config.otel.sample_rate, 1.0);
        assert!(config.otel.scrub_tool_inputs);
        assert!(config.otel.scrub_tool_outputs);
    }

    #[test]
    fn otel_rejects_invalid_sample_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad-otel.toml");
        let toml_str = format!(
            "{}\n[otel]\nenabled = true\nendpoint = \"http://localhost:4317\"\nsample_rate = 1.5\n",
            MINIMAL_TOML
        );
        std::fs::write(&path, toml_str).unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(
            err.to_string()
                .contains("otel.sample_rate must be between 0.0 and 1.0 inclusive"),
            "expected sample_rate error, got: {err}"
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

    #[test]
    fn env_overrides_rebase_data_dir_and_apply_runtime_values() {
        let mut config: KernelConfig = toml::from_str(MINIMAL_TOML).expect("config should parse");
        config.audit.log_path = "/tmp/agentos/data/audit.db".to_string();
        config.secrets.vault_path = "/tmp/agentos/data/vault.db".to_string();
        config.bus.socket_path = "/tmp/agentos/data/agentos.sock".to_string();
        config.tools.core_tools_dir = "/tmp/agentos/data/tools/core".to_string();
        config.tools.user_tools_dir = "/tmp/agentos/data/tools/user".to_string();
        config.kernel.state_db_path = "/tmp/agentos/data/kernel_state.db".to_string();
        config.memory.model_cache_dir = "/tmp/agentos/data/models".to_string();

        let overrides = std::collections::HashMap::from([
            ("AGENTOS_DATA_DIR", "/var/lib/agentos".to_string()),
            ("AGENTOS_HEALTH_PORT", "9191".to_string()),
            ("AGENTOS_OLLAMA_MODEL", "llama3.3".to_string()),
            (
                "AGENTOS_LLM_ANTHROPIC_BASE_URL",
                "https://anthropic.internal/v1".to_string(),
            ),
            (
                "AGENTOS_OPENAI_BASE_URL",
                "https://openai.internal/v1".to_string(),
            ),
        ]);

        apply_env_overrides_from(&mut config, |key| overrides.get(key).cloned());

        assert_eq!(config.tools.data_dir, "/var/lib/agentos");
        assert_eq!(config.audit.log_path, "/var/lib/agentos/audit.db");
        assert_eq!(config.secrets.vault_path, "/var/lib/agentos/vault.db");
        assert_eq!(config.bus.socket_path, "/var/lib/agentos/agentos.sock");
        assert_eq!(config.tools.core_tools_dir, "/var/lib/agentos/tools/core");
        assert_eq!(config.tools.user_tools_dir, "/var/lib/agentos/tools/user");
        assert_eq!(
            config.kernel.state_db_path,
            "/var/lib/agentos/kernel_state.db"
        );
        assert_eq!(config.memory.model_cache_dir, "/var/lib/agentos/models");
        assert_eq!(config.kernel.health_port, 9191);
        assert_eq!(config.ollama.default_model, "llama3.3");
        assert_eq!(
            config.llm.anthropic_base_url.as_deref(),
            Some("https://anthropic.internal/v1")
        );
        assert_eq!(
            config.llm.openai_base_url.as_deref(),
            Some("https://openai.internal/v1")
        );
    }
}
