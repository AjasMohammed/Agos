use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

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
    /// Skills system configuration.
    #[serde(default)]
    pub skills: SkillsConfig,
    /// OpenTelemetry export configuration.
    #[serde(default)]
    pub otel: OtelConfig,
    /// REST API server configuration.
    #[serde(default)]
    pub api: ApiSettings,
    /// User-selectable approval mode for tool calls. Controls when the
    /// kernel auto-approves vs. escalates a tool call for human review.
    #[serde(default)]
    pub approval: ApprovalConfig,
    /// Chat-specific kernel configuration (output filtering, enforcement modes).
    #[serde(default)]
    pub chat: ChatConfig,
    /// User preference adaptation post-task proposer.
    #[serde(default)]
    pub user_adaptation: UserAdaptationConfig,
    /// Structured user-profile store (proactive personalization L0/L1 source).
    #[serde(default)]
    pub user_profile: UserProfileConfig,
    /// Proactive personalization — read-back into context (L0/L1), the
    /// background interest model, and proactive recommendations (L2).
    #[serde(default)]
    pub personalization: PersonalizationConfig,
    /// Managed environment (`env-install`) policies and allowlists.
    /// Controls which packages agents may install into per-agent workspaces.
    #[serde(default)]
    pub env: EnvSettings,
    /// Gateway ("run as a bot") config — channels connected automatically at
    /// `agentos gateway run` boot. See `GatewaySettings`.
    #[serde(default)]
    pub gateway: GatewaySettings,
    /// Scheduler run-history retention.
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    /// Inbound voice/audio transcription (speech-to-text) for channel media.
    #[serde(default)]
    pub transcription: TranscriptionSettings,
}

/// Speech-to-text settings for inbound channel voice/audio messages.
///
/// When enabled, voice notes received on a channel (e.g. Telegram) are sent to
/// an OpenAI-compatible `/audio/transcriptions` endpoint and the transcript is
/// injected into the message text the agent reads. Disabled by default; the API
/// key is read from the named environment variable (never stored in config).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TranscriptionSettings {
    /// Master switch. When false, voice/audio is stored but not transcribed.
    #[serde(default)]
    pub enabled: bool,
    /// OpenAI-compatible transcription endpoint (multipart `file` + `model`).
    #[serde(default = "default_transcription_endpoint")]
    pub endpoint: String,
    /// Model name sent in the request (e.g. `whisper-1`, `whisper-large-v3`).
    #[serde(default = "default_transcription_model")]
    pub model: String,
    /// Environment variable holding the API key (Bearer auth). Never the key itself.
    #[serde(default = "default_transcription_key_env")]
    pub api_key_env: String,
}

impl Default for TranscriptionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_transcription_endpoint(),
            model: default_transcription_model(),
            api_key_env: default_transcription_key_env(),
        }
    }
}

fn default_transcription_endpoint() -> String {
    "https://api.openai.com/v1/audio/transcriptions".to_string()
}

fn default_transcription_model() -> String {
    "whisper-1".to_string()
}

fn default_transcription_key_env() -> String {
    "OPENAI_API_KEY".to_string()
}

/// Configuration for the scheduler's persisted run history.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SchedulerConfig {
    /// Days of per-fire run history to keep in `schedules.db`. The
    /// TimeoutChecker prunes completed/failed runs older than this on its
    /// periodic sweep. `0` disables pruning (unbounded growth — not advised).
    #[serde(default = "default_run_retention_days")]
    pub run_retention_days: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            run_retention_days: default_run_retention_days(),
        }
    }
}

fn default_run_retention_days() -> u32 {
    30
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserAdaptationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_user_adaptation_model")]
    pub model: String,
    #[serde(default = "default_user_adaptation_min_confidence")]
    pub min_confidence: f32,
    #[serde(default = "default_user_adaptation_max_proposals_per_task")]
    pub max_proposals_per_task: usize,
    /// Days that a pending proposal lives before the TimeoutChecker sweep
    /// transitions it to `expired` (history is preserved — no DELETE).
    #[serde(default = "default_user_adaptation_ttl_days")]
    pub proposal_ttl_days: i64,
}

impl Default for UserAdaptationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_user_adaptation_model(),
            min_confidence: default_user_adaptation_min_confidence(),
            max_proposals_per_task: default_user_adaptation_max_proposals_per_task(),
            proposal_ttl_days: default_user_adaptation_ttl_days(),
        }
    }
}

fn default_user_adaptation_model() -> String {
    "compact".to_string()
}
fn default_user_adaptation_min_confidence() -> f32 {
    0.5
}
fn default_user_adaptation_max_proposals_per_task() -> usize {
    3
}
fn default_user_adaptation_ttl_days() -> i64 {
    30
}

/// Structured user-profile store configuration (proactive personalization).
///
/// The profile store holds durable, categorized user preferences promoted from
/// accepted proposals. `max_pinned` and `min_confidence` are *also* enforced as
/// hard floors inside the store layer (see `user_profile_store`); the values
/// here are the operator-facing knobs. Enabled by default — the store simply
/// holds promoted prefs and is harmless empty; the *read-back into context*
/// (Phase 2) is what carries a separate, default-off `personalization` gate.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserProfileConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional override for the SQLite path; defaults to `{data_dir}/user_profile.db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_path: Option<String>,
    /// Maximum number of L0-pinned entries surfaced to context.
    #[serde(default = "default_user_profile_max_pinned")]
    pub max_pinned: i64,
    /// Minimum confidence for an entry to be stored (operator-facing floor).
    #[serde(default = "default_user_profile_min_confidence")]
    pub min_confidence: f32,
}

impl Default for UserProfileConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: None,
            max_pinned: default_user_profile_max_pinned(),
            min_confidence: default_user_profile_min_confidence(),
        }
    }
}

fn default_user_profile_max_pinned() -> i64 {
    8
}
fn default_user_profile_min_confidence() -> f32 {
    0.30
}

/// Proactive personalization configuration (shared by Phases 2–6).
///
/// Tiered by token cost:
/// - **L0** (read-back): the `enabled` master switch controls whether a compact
///   pinned profile block is injected into agent context. `profile_pin_cap` and
///   `profile_token_budget` bound its size.
/// - **L2** (background): the interest model + recommendation engine run off the
///   task path. `interest_*` fields tune the decaying interest aggregator;
///   `proactive_*` fields gate and rate-limit recommendations.
///
/// `enabled` is OFF by default — turning it on is the opt-in for putting profile
/// data into context. `proactive_enabled` is a *separate*, also-off-by-default
/// switch for outbound recommendations.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PersonalizationConfig {
    /// Master switch for read-back-into-context (L0/L1). Opt-in.
    #[serde(default)]
    pub enabled: bool,
    /// Max pinned entries surfaced in the L0 block.
    #[serde(default = "default_profile_pin_cap")]
    pub profile_pin_cap: usize,
    /// Hard token budget for the rendered L0 block.
    #[serde(default = "default_profile_token_budget")]
    pub profile_token_budget: usize,

    // --- L2: interest model (Phase 3) ---
    /// Half-life (hours) for interest-score exponential decay.
    #[serde(default = "default_interest_decay_half_life_hours")]
    pub interest_decay_half_life_hours: f64,
    /// Minimum decayed score below which an interest topic is pruned.
    #[serde(default = "default_interest_min_score")]
    pub interest_min_score: f64,
    /// Trigger: aggregate after this many task completions.
    #[serde(default = "default_interest_trigger_tasks")]
    pub interest_aggregation_trigger_tasks: u64,
    /// Trigger: aggregate after at least this many hours since the last cycle.
    #[serde(default = "default_interest_trigger_hours")]
    pub interest_aggregation_trigger_hours: f64,

    // --- L2: proactive recommendations (Phase 4) ---
    /// Separate opt-in for generating + delivering proactive recommendations.
    #[serde(default)]
    pub proactive_enabled: bool,
    /// Maximum recommendations delivered per day.
    #[serde(default = "default_max_recommendations_per_day")]
    pub max_recommendations_per_day: u32,
    /// Cooldown (hours) before an identical (deduped) recommendation may repeat.
    #[serde(default = "default_recommendation_dedup_cooldown_hours")]
    pub recommendation_dedup_cooldown_hours: f64,
    /// Minimum confidence for a recommendation to be delivered.
    #[serde(default = "default_recommendation_min_confidence")]
    pub recommendation_min_confidence: f32,

    // --- Phase 5: feedback loop ---
    /// Exponential half-life (days) for profile pin_rank decay.
    /// With 30d default an unused entry halves its rank every month.
    #[serde(default = "default_pin_rank_decay_half_life_days")]
    pub pin_rank_decay_half_life_days: f64,
    /// Archive Active profile entries idle for longer than this many days.
    #[serde(default = "default_profile_archive_idle_days")]
    pub profile_archive_idle_days: i64,
    /// Hours a dismissed recommendation's dedup_hash is suppressed (7d default).
    #[serde(default = "default_dismiss_cooldown_hours")]
    pub dismiss_cooldown_hours: i64,
    /// Confidence boost when the user re-states an existing preference (f32, clamped to 1.0).
    #[serde(default = "default_restate_confidence_boost")]
    pub restate_confidence_boost: f32,
}

impl Default for PersonalizationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            profile_pin_cap: default_profile_pin_cap(),
            profile_token_budget: default_profile_token_budget(),
            interest_decay_half_life_hours: default_interest_decay_half_life_hours(),
            interest_min_score: default_interest_min_score(),
            interest_aggregation_trigger_tasks: default_interest_trigger_tasks(),
            interest_aggregation_trigger_hours: default_interest_trigger_hours(),
            proactive_enabled: false,
            max_recommendations_per_day: default_max_recommendations_per_day(),
            recommendation_dedup_cooldown_hours: default_recommendation_dedup_cooldown_hours(),
            recommendation_min_confidence: default_recommendation_min_confidence(),
            pin_rank_decay_half_life_days: default_pin_rank_decay_half_life_days(),
            profile_archive_idle_days: default_profile_archive_idle_days(),
            dismiss_cooldown_hours: default_dismiss_cooldown_hours(),
            restate_confidence_boost: default_restate_confidence_boost(),
        }
    }
}

fn default_profile_pin_cap() -> usize {
    8
}
fn default_profile_token_budget() -> usize {
    300
}
fn default_interest_decay_half_life_hours() -> f64 {
    // ~2 weeks: interests fade but aren't forgotten between sessions.
    336.0
}
fn default_interest_min_score() -> f64 {
    0.05
}
fn default_interest_trigger_tasks() -> u64 {
    25
}
fn default_interest_trigger_hours() -> f64 {
    24.0
}
fn default_max_recommendations_per_day() -> u32 {
    3
}
fn default_recommendation_dedup_cooldown_hours() -> f64 {
    168.0 // one week
}
fn default_recommendation_min_confidence() -> f32 {
    0.5
}
fn default_pin_rank_decay_half_life_days() -> f64 {
    30.0
}
fn default_profile_archive_idle_days() -> i64 {
    60
}
fn default_dismiss_cooldown_hours() -> i64 {
    168 // 7 days
}
fn default_restate_confidence_boost() -> f32 {
    0.10
}

/// Chat-specific kernel configuration.
///
/// Controls server-side filtering applied to LLM output before it reaches the
/// chat UI. See `output_sanitizer` and the Output Sanitization plan for the
/// full filter pipeline.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ChatConfig {
    /// When true, the chat output filter only emits text appearing inside
    /// `<final>...</final>` tags. The system prompt is updated to instruct
    /// the model to wrap its visible answer in those tags. `<think>...</think>`
    /// blocks are always suppressed regardless of this setting.
    ///
    /// Off by default — flipping this on is a behavioral change that requires
    /// the connected LLM to follow the convention or the user gets an
    /// empty-answer placeholder reply.
    #[serde(default)]
    pub enforce_final_tag: bool,

    /// Maximum tool-call iterations per chat session before the loop is forced
    /// to terminate with a "limit reached" note. One iteration covers a single
    /// LLM inference + any tool calls it requested. Small models that thrash
    /// on meta-tools (search-tools/describe-tool/agent-manual) need headroom
    /// before reaching real action; raise this if chat sessions hit the cap.
    #[serde(default = "default_chat_max_tool_iterations")]
    pub max_tool_iterations: u32,
}

fn default_chat_max_tool_iterations() -> u32 {
    25
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
    pub secret: Zeroizing<String>,
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
            secret: Zeroizing::new(String::new()),
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
    /// Tunables for the iteration-loop context compactor (see
    /// `crates/agentos-kernel/src/context_compactor.rs`). Without this
    /// block the kernel uses sane defaults; operators tuning long-running
    /// agentic workflows can override `cadence` (compact every N
    /// iterations), `keep_recent_iterations` (rolling window kept fresh),
    /// or disable LLM summarization to reduce per-iteration latency.
    #[serde(default)]
    pub context_compaction: ContextCompactionConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextCompactionConfig {
    /// Cadence in completed iterations. The compactor self-gates: it
    /// only fires when `completed_iterations` is a non-zero multiple of
    /// this value AND there are enough compactable entries.
    #[serde(default = "default_compaction_cadence")]
    pub cadence: usize,
    /// How many recent iteration's worth of entries to keep verbatim.
    /// Older entries are summarized into a rolling `[ROLLING TASK
    /// SUMMARY]` block. The compactor multiplies this by 4 to derive
    /// the entry count.
    #[serde(default = "default_compaction_keep_recent")]
    pub keep_recent_iterations: usize,
    /// When true, the compactor calls the agent's current LLM adapter
    /// to generate a coherent summary; on any LLM failure it transparently
    /// falls back to the extractive heuristic. Operators on tight latency
    /// budgets or running unreliable local models can set this to false.
    #[serde(default = "default_enable_llm_compaction")]
    pub enable_llm_summarization: bool,
}

impl Default for ContextCompactionConfig {
    fn default() -> Self {
        Self {
            cadence: default_compaction_cadence(),
            keep_recent_iterations: default_compaction_keep_recent(),
            enable_llm_summarization: default_enable_llm_compaction(),
        }
    }
}

fn default_compaction_cadence() -> usize {
    4
}

fn default_compaction_keep_recent() -> usize {
    2
}

fn default_enable_llm_compaction() -> bool {
    true
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
    /// `host-package-install` tool configuration. Disabled by default.
    #[serde(default)]
    pub host_package: HostPackageSettings,
    /// Tool-discovery (Tier-0 index) tuning.
    #[serde(default)]
    pub discovery: DiscoverySettings,
}

/// Tuning for the Tier-0 tool-discovery index (the compact per-turn `Tools`
/// line). See `obsidian-vault/plans/tool-discovery-architecture`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiscoverySettings {
    /// Max usage-ranked tool names shown per category in the Tier-0 line.
    #[serde(default = "default_l0_max_names")]
    pub l0_max_names_per_category: usize,
    /// Soft token budget for the Tier-0 line's names portion (≈4 chars/token);
    /// categories beyond the budget render counts-only.
    #[serde(default = "default_l0_max_tokens")]
    pub l0_max_tokens: usize,
    /// Scope the native tool array to task-relevant categories (Phase 3).
    /// Default ON. Anything scoped out stays reachable via `search-tools`.
    #[serde(default = "default_true")]
    pub default_scoping: bool,
    /// Task classifier: "heuristic" | "heuristic+semantic" | "llm".
    /// Only "heuristic" is implemented today; others degrade to heuristic.
    #[serde(default = "default_scoping_classifier")]
    pub scoping_classifier: String,
}

fn default_l0_max_names() -> usize {
    5
}

fn default_l0_max_tokens() -> usize {
    200
}

fn default_scoping_classifier() -> String {
    "heuristic".to_string()
}

impl Default for DiscoverySettings {
    fn default() -> Self {
        Self {
            l0_max_names_per_category: default_l0_max_names(),
            l0_max_tokens: default_l0_max_tokens(),
            default_scoping: true,
            scoping_classifier: default_scoping_classifier(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HostPackageSettings {
    /// Master kill-switch. When `false`, the tool is registered but every
    /// call returns "no privilege escalator configured" — no host packages
    /// can be installed regardless of allowlist or approval.
    #[serde(default)]
    pub enabled: bool,
    /// Privilege escalator policy. One of: "auto", "pkexec", "helper", "none".
    #[serde(default = "default_host_package_escalator")]
    pub privilege_escalator: String,
    /// Path to the setuid helper (used when `privilege_escalator = "helper"`).
    #[serde(default = "default_host_package_helper_path")]
    pub helper_path: String,
    /// Package managers to detect on PATH (in priority order).
    #[serde(default = "default_host_package_managers")]
    pub managers: Vec<String>,
    /// Operator-controlled allowlist. Only packages whose names match an
    /// entry verbatim may be installed, even after user approval.
    #[serde(default)]
    pub allowlist: Vec<String>,
}

impl Default for HostPackageSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            privilege_escalator: default_host_package_escalator(),
            helper_path: default_host_package_helper_path(),
            managers: default_host_package_managers(),
            allowlist: Vec::new(),
        }
    }
}

fn default_host_package_escalator() -> String {
    "auto".into()
}

fn default_host_package_helper_path() -> String {
    "/usr/local/libexec/agentos-pkg-helper".into()
}

fn default_host_package_managers() -> Vec<String> {
    vec![
        "apt-get".into(),
        "dnf".into(),
        "pacman".into(),
        "zypper".into(),
        "apk".into(),
        "brew".into(),
    ]
}

/// Managed environments (`env-install`) configuration.
///
/// Controls which packages agents may install into per-agent workspaces at
/// `{data_dir}/workspaces/{agent_id}/{name}/`. Workspaces are isolated per
/// agent and never touch the host system package set.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnvSettings {
    /// Python package policy: `"curated"` (allowlist), `"open"` (any), or `"locked"` (none).
    #[serde(default = "default_env_policy")]
    pub python_policy: String,
    /// Node.js package policy: `"curated"`, `"open"`, or `"locked"`.
    #[serde(default = "default_env_policy")]
    pub nodejs_policy: String,
    /// Rust (cargo install) policy: `"curated"`, `"open"`, or `"locked"`.
    #[serde(default = "default_env_policy")]
    pub rust_policy: String,
    /// System package policy. Locked by default; host packages route through
    /// `host-package-install` instead.
    #[serde(default = "default_locked_policy")]
    pub system_policy: String,
    /// Per-agent workspace disk quota in bytes (informational; not enforced yet).
    #[serde(default = "default_env_quota")]
    pub default_quota_bytes: u64,
    /// Maximum wall time for a single install command.
    #[serde(default = "default_env_install_timeout")]
    pub install_timeout_secs: u64,
    /// Curated Python allowlist (only consulted when `python_policy = "curated"`).
    #[serde(default)]
    pub python_allowlist: Vec<String>,
    /// Curated Node.js allowlist.
    #[serde(default)]
    pub nodejs_allowlist: Vec<String>,
    /// Curated Rust allowlist.
    #[serde(default)]
    pub rust_allowlist: Vec<String>,
}

fn default_env_policy() -> String {
    "curated".into()
}
fn default_locked_policy() -> String {
    "locked".into()
}
fn default_env_quota() -> u64 {
    2_147_483_648 // 2 GiB
}
fn default_env_install_timeout() -> u64 {
    120
}

impl Default for EnvSettings {
    fn default() -> Self {
        Self {
            python_policy: default_env_policy(),
            nodejs_policy: default_env_policy(),
            rust_policy: default_env_policy(),
            system_policy: default_locked_policy(),
            default_quota_bytes: default_env_quota(),
            install_timeout_secs: default_env_install_timeout(),
            python_allowlist: Vec::new(),
            nodejs_allowlist: Vec::new(),
            rust_allowlist: Vec::new(),
        }
    }
}

/// Gateway ("run as a bot") configuration. When `enabled`, `agentos gateway
/// run` connects each channel in `channels` at boot through the same path as
/// `agentos channel connect`. Tokens are referenced by a vault `credential_key`
/// — never inline.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GatewaySettings {
    /// Master switch. When false, `gateway run` boots the kernel (restoring any
    /// previously `channel connect`-ed channels) but connects nothing new.
    #[serde(default)]
    pub enabled: bool,
    /// Channels to connect declaratively at gateway boot.
    #[serde(default)]
    pub channels: Vec<GatewayChannelConfig>,
}

/// One channel to connect at gateway boot. Fields map 1:1 to the
/// `ConnectChannel` command consumed by `build_channel_adapter`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayChannelConfig {
    /// Channel kind (must be a known `ChannelKind`; an unknown kind fails the
    /// gateway boot): telegram | ntfy | email | discord | slack | whatsapp | webhook
    pub kind: String,
    /// Channel-specific external id (e.g. Telegram chat_id). Omit for Telegram
    /// to auto-discover from the first `/start`.
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub display_name: String,
    /// VAULT key holding the token — never an inline token. Seed it first with
    /// `agentos secret set <key> <token>`.
    #[serde(default)]
    pub credential_key: String,
    #[serde(default)]
    pub reply_topic: Option<String>,
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Default agent for inbound chat on this channel.
    #[serde(default)]
    pub active_agent: Option<String>,
    /// Set false to declare-but-skip this channel.
    #[serde(default = "default_true")]
    pub enabled: bool,
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
    /// Ordered fallback chain applied to every agent's primary adapter. When the
    /// primary provider errors (including mid-stream), the kernel fails over to
    /// these in order via `FallbackAdapter`. Empty by default — no behavior
    /// change unless configured. Entries that fail to build (e.g. missing key)
    /// are skipped at construction time with a warning rather than failing the
    /// agent.
    #[serde(default)]
    pub fallback_models: Vec<FallbackModelConfig>,
}

/// One entry in `llm.fallback_models`. Mirrors the `--provider`/`--model`
/// arguments used when connecting an agent.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct FallbackModelConfig {
    /// Provider name: `ollama` | `openai` | `anthropic` | `gemini`, a catalog
    /// name like `nvidia`, or `custom:<name>`.
    pub provider: String,
    /// Model identifier for that provider.
    pub model: String,
    /// Optional base URL override (defaults resolve from the catalog/config).
    #[serde(default)]
    pub base_url: Option<String>,
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
            fallback_models: Vec::new(),
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
    /// Skip loading the ONNX embedding model at boot.
    ///
    /// When `true`, the kernel constructs a no-op embedder that returns
    /// zero vectors instead of initializing fastembed/onnxruntime. Memory
    /// stores still operate (FTS5 lexical search keeps working), but
    /// vector-based retrieval becomes a no-op. Intended for environments
    /// where onnxruntime crashes during graph optimization (e.g. certain
    /// Zen 3 / glibc combinations trigger an integer-divide-by-zero in the
    /// transpose-optimizer hashmap) or for tests that boot the kernel
    /// without needing semantic retrieval.
    #[serde(default)]
    pub disable_embedder: bool,
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
            disable_embedder: false,
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

/// Configuration for a single external MCP server process or HTTP endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    // ── Identity ──────────────────────────────────────────────────────────
    /// Human-readable name for this server (used in logs, CLI, status).
    pub name: String,

    // ── Stdio Transport ───────────────────────────────────────────────────
    /// Path or name of the executable to spawn (e.g. `"npx"`, `"python3"`).
    /// Set for stdio transport. Mutually exclusive with `url`.
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Additional environment variables for the subprocess.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Working directory for the subprocess.
    #[serde(default)]
    pub working_dir: Option<std::path::PathBuf>,

    // ── HTTP Transport ────────────────────────────────────────────────────
    /// MCP server endpoint URL (e.g. `"http://localhost:8080/mcp"`).
    /// Set for HTTP transport. Mutually exclusive with `command`.
    #[serde(default)]
    pub url: Option<String>,
    /// Bearer token for HTTP authentication (plaintext).
    #[serde(default)]
    pub auth_token: Option<String>,

    // ── Security ──────────────────────────────────────────────────────────
    /// Trust tier: `"community"` (default) or `"verified"`.
    #[serde(default = "default_mcp_trust_tier")]
    pub trust_tier: String,
    /// Max response size in bytes. Overrides global default (1MB).
    #[serde(default)]
    pub max_response_bytes: Option<usize>,
    /// Rate limit: max calls per minute to this server.
    #[serde(default)]
    pub rate_limit_rpm: Option<u32>,
    /// Tool whitelist (empty = allow all).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tool blacklist (takes precedence over allow list).
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// Per-request timeout in seconds. Overrides global default (30s).
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    // ── Lifecycle ─────────────────────────────────────────────────────────
    /// Whether to automatically reconnect on connection failure. Default: true.
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Health check interval in seconds. Default: 30.
    #[serde(default = "default_mcp_health_check_interval")]
    pub health_check_interval_secs: u64,
}

fn default_mcp_trust_tier() -> String {
    "community".to_string()
}

fn default_mcp_health_check_interval() -> u64 {
    30
}

impl McpServerConfig {
    /// Validate the config.
    pub fn validate(&self) -> Result<(), String> {
        let has_command = self.command.is_some();
        let has_url = self.url.is_some();

        match (has_command, has_url) {
            (true, true) => {
                return Err(format!(
                    "MCP server '{}': cannot set both 'command' and 'url'",
                    self.name
                ))
            }
            (false, false) => {
                return Err(format!(
                    "MCP server '{}': must set either 'command' (stdio) or 'url' (HTTP)",
                    self.name
                ))
            }
            _ => {}
        }

        if let Some(ref url) = self.url {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(format!(
                    "MCP server '{}': url must start with http:// or https://, got '{}'",
                    self.name, url
                ));
            }
        }

        if self.trust_tier != "community" && self.trust_tier != "verified" {
            return Err(format!(
                "MCP server '{}': trust_tier must be 'community' or 'verified', got '{}'",
                self.name, self.trust_tier
            ));
        }

        if self.health_check_interval_secs == 0 {
            return Err(format!(
                "MCP server '{}': health_check_interval_secs must be > 0",
                self.name
            ));
        }

        Ok(())
    }

    /// Infer the transport type based on config.
    pub fn transport_type(&self) -> Option<&'static str> {
        match (&self.command, &self.url) {
            (Some(_), None) => Some("stdio"),
            (None, Some(_)) => Some("http"),
            _ => None,
        }
    }
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
/// Controls where `agentos tool search/add/publish` connect to fetch and
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

/// Skills system configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillsConfig {
    /// Directory containing core (bundled) skills.
    #[serde(default = "default_core_skills_dir")]
    pub core_skills_dir: String,
    /// Directory containing user-installed skills.
    #[serde(default = "default_user_skills_dir")]
    pub user_skills_dir: String,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            core_skills_dir: default_core_skills_dir(),
            user_skills_dir: default_user_skills_dir(),
        }
    }
}

fn default_core_skills_dir() -> String {
    "skills/core".to_string()
}

fn default_user_skills_dir() -> String {
    "skills/user".to_string()
}

/// REST API server configuration.
///
/// When `enabled` is true, the kernel boots an HTTP API server alongside the
/// Unix domain socket bus. The API server provides programmatic access to
/// agents, tasks, tools, secrets, pipelines, audit, and notifications.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApiSettings {
    /// Whether to start the API server on kernel boot.
    #[serde(default = "default_false")]
    pub enabled: bool,
    /// Host address to bind the API server to.
    #[serde(default = "default_api_host")]
    pub host: String,
    /// TCP port for the API server.
    #[serde(default = "default_api_port")]
    pub port: u16,
    /// Whether to serve the interactive Scalar API-docs UI at `GET /api/v1/docs`.
    /// The `GET /api/v1/openapi.json` contract endpoint stays public regardless.
    /// Disable on internet-exposed deployments.
    #[serde(default = "default_true")]
    pub docs_enabled: bool,
    /// Operator credential for `POST /api/v1/auth/login`. When unset (or empty),
    /// the login endpoint is disabled and returns `503`. Set this to let a
    /// browser SPA exchange the operator credential for a scoped, expiring key.
    #[serde(default)]
    pub operator_token: Option<String>,
    /// Cross-origin allowlist for the REST API. Each entry is a full origin
    /// (scheme + host + optional port), e.g. `http://localhost:5173`. When empty,
    /// CORS falls back to the API's own bind origin (same-origin only).
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
    /// Whether `POST /api/v1/auth/refresh` (key rotation) is enabled.
    #[serde(default = "default_false")]
    pub refresh_enabled: bool,
    /// Whether `PUT /api/v1/config/{key}` may write to the config file at runtime.
    /// Off by default — enable only on trusted control-plane deployments.
    #[serde(default = "default_false")]
    pub config_writable: bool,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: default_api_host(),
            port: default_api_port(),
            docs_enabled: true,
            operator_token: None,
            cors_allowed_origins: Vec::new(),
            refresh_enabled: false,
            config_writable: false,
        }
    }
}

/// `[approval]` config block. Controls when the kernel auto-approves vs.
/// escalates a tool call for human review.
///
/// `mode` is the global default. `agent_overrides` lets the operator dial
/// up or down for individual agents — e.g. a research-bot in `auto` next to
/// a writer-bot in `ask_always` inside the same kernel.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ApprovalConfig {
    /// Global default mode. Defaults to `ask_edit` (Claude-Code-style:
    /// auto-approve reads, prompt for writes/exec/control-plane).
    #[serde(default)]
    pub mode: agentos_types::ApprovalMode,
    /// Per-agent overrides keyed by display name. Lookup is name-based so
    /// operators can express the override in TOML without knowing UUIDs.
    #[serde(default)]
    pub agent_overrides: std::collections::BTreeMap<String, agentos_types::ApprovalMode>,
}

fn default_api_host() -> String {
    "127.0.0.1".to_string()
}

fn default_api_port() -> u16 {
    8080
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

/// Validate that all MCP server entries have non-empty, unique names and valid config.
fn validate_mcp_config(mcp: &McpConfig) -> Result<(), anyhow::Error> {
    let mut seen_names = std::collections::HashSet::new();
    for (i, srv) in mcp.servers.iter().enumerate() {
        if srv.name.trim().is_empty() {
            anyhow::bail!("mcp.servers[{}]: 'name' must not be empty", i);
        }
        if !seen_names.insert(srv.name.clone()) {
            anyhow::bail!("mcp.servers[{}]: duplicate server name '{}'", i, srv.name);
        }
        if let Err(e) = srv.validate() {
            anyhow::bail!("mcp.servers[{}] ({}): {}", i, srv.name, e);
        }
    }
    Ok(())
}

/// Roots that are never allowed as a workspace path, not even as a deep subpath.
/// Mounting any of these would expose security-critical OS state.
pub(crate) const WORKSPACE_FORBIDDEN_ROOTS: &[&str] = &[
    "/etc", "/proc", "/sys", "/dev", "/boot", "/usr", "/bin", "/sbin", "/lib", "/lib64",
];

/// Roots that are too broad as exact paths, but whose subpaths ARE allowed.
/// For example `/home` is rejected but `/home/alice/project` is fine.
pub(crate) const WORKSPACE_BARE_FORBIDDEN: &[&str] = &[
    "/", "/home", "/var", "/root", "/tmp", "/mnt", "/media", "/opt",
];

/// Validate a single workspace path. Public so the runtime grant store can reuse it.
pub(crate) fn validate_workspace_path(path_str: &str) -> Result<(), anyhow::Error> {
    let p = std::path::Path::new(path_str);
    if !p.is_absolute() {
        anyhow::bail!(
            "workspace path '{}' is not absolute; must start with '/'",
            path_str
        );
    }
    // Reject `..` components defensively; lexical only — caller may also canonicalize.
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!(
            "workspace path '{}' contains '..' component; resolve it first",
            path_str
        );
    }
    for root in WORKSPACE_FORBIDDEN_ROOTS {
        let root_p = std::path::Path::new(root);
        if p == root_p || p.starts_with(root_p) {
            anyhow::bail!(
                "workspace path '{}' is under forbidden system root '{}'",
                path_str,
                root
            );
        }
    }
    for root in WORKSPACE_BARE_FORBIDDEN {
        if p == std::path::Path::new(root) {
            anyhow::bail!(
                "workspace path '{}' is too broad; grant a specific subdirectory instead",
                path_str
            );
        }
    }
    // Must have at least one path component beyond the filesystem root.
    let components: Vec<_> = p.components().collect();
    if components.len() < 2 {
        anyhow::bail!(
            "workspace path '{}' is too broad — must include at least one subdirectory",
            path_str
        );
    }
    Ok(())
}

/// Validate that all workspace paths in config are absolute and not forbidden.
fn validate_workspace_paths(workspace: &WorkspaceConfig) -> Result<(), anyhow::Error> {
    for path_str in &workspace.allowed_paths {
        validate_workspace_path(path_str)
            .map_err(|e| anyhow::anyhow!("tools.workspace.allowed_paths: {}", e))?;
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

    // Logging overrides — containers/systemd commonly set these via env.
    // Invalid values are rejected by validate_logging_settings after overrides apply.
    apply_string_override(
        &lookup,
        "AGENTOS_LOG_FORMAT",
        &mut config.logging.log_format,
    );
    apply_string_override(&lookup, "AGENTOS_LOG_LEVEL", &mut config.logging.log_level);

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
    fn validate_workspace_path_accepts_user_subdirs() {
        assert!(validate_workspace_path("/home/alice/Desktop").is_ok());
        assert!(validate_workspace_path("/home/alice/projects/foo").is_ok());
        assert!(validate_workspace_path("/var/log/agentos").is_ok());
        assert!(validate_workspace_path("/tmp/work").is_ok());
        assert!(validate_workspace_path("/opt/agentos-data").is_ok());
        assert!(validate_workspace_path("/mnt/external/data").is_ok());
    }

    #[test]
    fn validate_workspace_path_rejects_bare_broad_roots() {
        assert!(validate_workspace_path("/").is_err());
        assert!(validate_workspace_path("/home").is_err());
        assert!(validate_workspace_path("/var").is_err());
        assert!(validate_workspace_path("/root").is_err());
        assert!(validate_workspace_path("/tmp").is_err());
        assert!(validate_workspace_path("/opt").is_err());
    }

    #[test]
    fn validate_workspace_path_rejects_forbidden_roots_and_subpaths() {
        for bad in &[
            "/etc",
            "/etc/agentos",
            "/proc",
            "/proc/1",
            "/sys/kernel",
            "/dev/sda",
            "/boot/grub",
            "/usr/bin",
            "/bin/sh",
            "/sbin/init",
            "/lib/x86_64-linux-gnu",
            "/lib64/ld-linux-x86-64.so.2",
        ] {
            assert!(
                validate_workspace_path(bad).is_err(),
                "expected '{}' to be rejected",
                bad
            );
        }
    }

    #[test]
    fn validate_workspace_path_rejects_relative_and_traversal() {
        assert!(validate_workspace_path("relative/path").is_err());
        assert!(validate_workspace_path("../escape").is_err());
        assert!(validate_workspace_path("/home/alice/../bob").is_err());
    }

    #[test]
    fn production_toml_otel_logging_valid() {
        // Guards the shipped production profile: structured JSON logs on, OTel
        // opt-in (disabled), and the preflight block present. Catches a
        // fat-fingered [logging]/[otel]/[preflight] regression that would still
        // parse as valid TOML but ship the wrong production defaults.
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/production.toml");
        // Parse the shipped file directly (NOT load_config) so the assertions test
        // the file's defaults rather than the resolved process environment — env
        // overrides like AGENTOS_OTEL_ENABLED must not be able to flip this test.
        let content = std::fs::read_to_string(&path).expect("config/production.toml must exist");
        let cfg: KernelConfig =
            toml::from_str(&content).expect("config/production.toml must parse");
        assert_eq!(cfg.logging.log_format, "json");
        assert!(!cfg.otel.enabled, "otel must default to disabled (opt-in)");
        assert_eq!(cfg.preflight.min_free_disk_mb, 512);
        assert!(cfg.preflight.check_db_writable);
    }

    #[test]
    fn default_config_tools_discovery_parses() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/default.toml");
        let content = std::fs::read_to_string(&path).expect("config/default.toml must exist");
        let cfg: KernelConfig = toml::from_str(&content)
            .expect("config/default.toml must parse with [tools.discovery]");
        assert_eq!(cfg.tools.discovery.l0_max_names_per_category, 5);
        assert_eq!(cfg.tools.discovery.l0_max_tokens, 200);
    }

    #[test]
    fn discovery_settings_defaults_when_absent() {
        // A [tools] block without [tools.discovery] must still parse (serde
        // default), so existing deployments' configs keep working.
        let toml_str = r#"
core_tools_dir = "/t/c"
user_tools_dir = "/t/u"
data_dir = "/t/d"
"#;
        let tools: ToolsSettings =
            toml::from_str(toml_str).expect("tools without discovery parses");
        assert_eq!(tools.discovery.l0_max_names_per_category, 5);
        assert_eq!(tools.discovery.l0_max_tokens, 200);
    }

    #[test]
    fn gateway_settings_toml_roundtrip() {
        // The [gateway] contract: enabled flag + per-channel tables; per-channel
        // `enabled` defaults to true; absence of [gateway] is valid (disabled).
        let gw: GatewaySettings = toml::from_str(
            r#"
enabled = true
[[channels]]
kind = "telegram"
display_name = "Ops Bot"
credential_key = "tg_token"
active_agent = "assistant"
"#,
        )
        .expect("gateway block must parse");
        assert!(gw.enabled);
        assert_eq!(gw.channels.len(), 1);
        assert_eq!(gw.channels[0].kind, "telegram");
        assert_eq!(gw.channels[0].credential_key, "tg_token");
        assert!(
            gw.channels[0].enabled,
            "per-channel enabled defaults to true"
        );

        // Absent gateway block → default (disabled, no channels).
        let def = GatewaySettings::default();
        assert!(!def.enabled);
        assert!(def.channels.is_empty());
    }

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
    fn llm_settings_fallback_models_default_empty() {
        let config: KernelConfig = toml::from_str(MINIMAL_TOML).expect("config should parse");
        assert!(config.llm.fallback_models.is_empty());
    }

    #[test]
    fn llm_settings_parses_fallback_models() {
        let toml_str = format!(
            "{}\n[[llm.fallback_models]]\nprovider = \"ollama\"\nmodel = \"llama3.1:8b\"\n\
             \n[[llm.fallback_models]]\nprovider = \"anthropic\"\nmodel = \"claude-haiku-4-5\"\nbase_url = \"https://x/v1\"\n",
            MINIMAL_TOML
        );
        let config: KernelConfig = toml::from_str(&toml_str).expect("config should parse");
        assert_eq!(config.llm.fallback_models.len(), 2);
        assert_eq!(config.llm.fallback_models[0].provider, "ollama");
        assert_eq!(config.llm.fallback_models[0].model, "llama3.1:8b");
        assert_eq!(config.llm.fallback_models[0].base_url, None);
        assert_eq!(config.llm.fallback_models[1].provider, "anthropic");
        assert_eq!(
            config.llm.fallback_models[1].base_url.as_deref(),
            Some("https://x/v1")
        );
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
