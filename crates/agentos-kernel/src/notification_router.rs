use crate::config::{SlackAdapterConfig, WebhookAdapterConfig};
use crate::user_inbox::UserInbox;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_types::{
    AgentID, AgentOSError, ChannelInstanceID, DeliveryChannel, DeliveryStatus, NotificationID,
    NotificationPriority, NotificationSource, TraceID, UserMessage, UserMessageKind, UserResponse,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};

/// Maximum fire-and-forget notifications per agent per minute.
const RATE_LIMIT_NOTIFY_PER_MIN: u32 = 10;

/// Internal state for per-agent notification rate limiting.
struct RateLimiterState {
    count: u32,
    window_start: chrono::DateTime<Utc>,
}

/// Error type surfaced only within the delivery subsystem.
#[derive(Debug)]
pub struct DeliveryError(pub String);

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A message received from a user via an external bidirectional channel.
///
/// Produced by `DeliveryAdapter::start_listening` and routed by `InboundRouter`.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub channel: DeliveryChannel,
    /// The `ChannelInstanceID` of the registered channel this arrived on.
    pub channel_instance_id: ChannelInstanceID,
    /// Channel-specific sender ID (Telegram chat_id, ntfy topic, email address).
    pub external_sender_id: String,
    pub text: String,
    /// Set when the text matches the reply to a pending `Question` notification.
    pub reply_to_notification_id: Option<NotificationID>,
    pub received_at: DateTime<Utc>,
    /// Raw adapter-specific payload (for debugging / future use).
    pub raw: serde_json::Value,
}

/// Pluggable delivery channel adapter.
///
/// Each adapter handles one delivery channel (CLI, Web SSE, Webhook, …).
/// The `NotificationRouter` calls `deliver` on every available adapter after
/// writing the message to the `UserInbox`.
///
/// Adapters that support receiving inbound messages from the user implement
/// `supports_inbound() → true` and `start_listening(tx)`.
#[async_trait]
pub trait DeliveryAdapter: Send + Sync {
    fn channel_id(&self) -> DeliveryChannel;
    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError>;
    async fn is_available(&self) -> bool;

    // ── Phase 6: inbound support ─────────────────────────────────────────────

    /// Whether this adapter can receive messages from the user.
    ///
    /// Defaults to `false`.  Override to `true` for bidirectional adapters
    /// (Telegram, ntfy, …).
    fn supports_inbound(&self) -> bool {
        false
    }

    /// Unique instance identifier for channel adapters.
    ///
    /// Returns `Some(channel_instance_id.to_string())` for adapters registered
    /// via `cmd_connect_channel` so they can be removed by `deregister_adapter`.
    /// Returns `None` for built-in adapters (CLI, SSE, Webhook, Slack, Desktop).
    fn adapter_instance_id(&self) -> Option<String> {
        None
    }

    /// Start the background listener.
    ///
    /// The adapter spawns a task that forwards every inbound message to `tx`
    /// and returns the `JoinHandle` for the spawned task.  The caller stores
    /// the handle in `ChannelListenerRegistry` so it can be aborted on disconnect.
    ///
    /// Only called when `supports_inbound()` returns `true`.
    async fn start_listening(
        &self,
        _tx: mpsc::Sender<InboundMessage>,
    ) -> Result<tokio::task::JoinHandle<()>, DeliveryError> {
        Err(DeliveryError("inbound not supported".into()))
    }
}

/// Kernel subsystem that receives `UserMessage` objects from agents or kernel
/// internals and dispatches them to all registered delivery adapters while
/// persisting every message in the `UserInbox`.
///
/// This is the single authoritative dispatcher — delivery adapters are leaf
/// nodes that have no knowledge of each other.
pub struct NotificationRouter {
    inbox: Arc<UserInbox>,
    audit: Arc<agentos_audit::AuditLog>,
    adapters: RwLock<Vec<Arc<dyn DeliveryAdapter>>>,
    /// Pending oneshot senders for blocking `ask_user` questions.
    /// Key: the `NotificationID` of the Question message.
    waiting_tasks: Arc<RwLock<HashMap<NotificationID, oneshot::Sender<UserResponse>>>>,
    /// Per-agent rate limiter state.
    rate_limiter: Arc<RwLock<HashMap<AgentID, RateLimiterState>>>,
}

impl NotificationRouter {
    pub fn new(inbox: Arc<UserInbox>, audit: Arc<agentos_audit::AuditLog>) -> Self {
        Self {
            inbox,
            audit,
            adapters: RwLock::new(vec![Arc::new(CliDeliveryAdapter)]),
            waiting_tasks: Arc::new(RwLock::new(HashMap::new())),
            rate_limiter: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add a delivery adapter.  Called once during kernel startup or on channel connect.
    pub async fn register_adapter(&self, adapter: Arc<dyn DeliveryAdapter>) {
        self.adapters.write().await.push(adapter);
    }

    /// Remove a channel adapter by its `adapter_instance_id`.
    /// Called when a channel is disconnected via `cmd_disconnect_channel`.
    pub async fn deregister_adapter(&self, instance_id: &str) {
        self.adapters
            .write()
            .await
            .retain(|a| a.adapter_instance_id().as_deref() != Some(instance_id));
    }

    /// Deliver a message:
    /// 1. Rate-limit check.
    /// 2. Persist to `UserInbox`.
    /// 3. Fan out to all available adapters.
    /// 4. If `msg.interaction.blocking == true`, register a oneshot channel and
    ///    return `Some(rx)` so the caller can await the user's reply.
    pub async fn deliver(
        &self,
        msg: UserMessage,
    ) -> Result<Option<oneshot::Receiver<UserResponse>>, AgentOSError> {
        // Rate-limit enforcement for agent-sourced messages.
        self.check_rate_limit(&msg.from).await?;

        // Write to inbox first so the message survives even if delivery fails.
        self.inbox.write(&msg).await?;

        // Register a oneshot channel for blocking interactions before delivery
        // so we don't miss a response that arrives before delivery completes.
        let maybe_rx = if msg.interaction.as_ref().is_some_and(|i| i.blocking) {
            let (tx, rx) = oneshot::channel();
            self.waiting_tasks.write().await.insert(msg.id, tx);
            Some(rx)
        } else {
            None
        };

        // Fan out to all available adapters (best-effort; failures are logged).
        let adapters = self.adapters.read().await;
        for adapter in adapters.iter() {
            if !adapter.is_available().await {
                self.inbox
                    .update_delivery_status(&msg.id, adapter.channel_id(), DeliveryStatus::Skipped)
                    .await
                    .ok();
                continue;
            }
            match adapter.deliver(&msg).await {
                Ok(()) => {
                    let delivered_at = Utc::now();
                    self.inbox
                        .update_delivery_status(
                            &msg.id,
                            adapter.channel_id(),
                            DeliveryStatus::Delivered { at: delivered_at },
                        )
                        .await
                        .ok();
                    let _ = self.audit.append(AuditEntry {
                        timestamp: delivered_at,
                        trace_id: TraceID::new(),
                        event_type: AuditEventType::NotificationDelivered,
                        agent_id: None,
                        task_id: msg.task_id,
                        tool_id: None,
                        details: serde_json::json!({
                            "notification_id": msg.id.to_string(),
                            "channel": adapter.channel_id().to_string(),
                        }),
                        severity: AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        notification_id = %msg.id,
                        channel = %adapter.channel_id(),
                        error = %e,
                        "Notification delivery failed on channel"
                    );
                    self.inbox
                        .update_delivery_status(
                            &msg.id,
                            adapter.channel_id(),
                            DeliveryStatus::Failed {
                                reason: e.0.clone(),
                            },
                        )
                        .await
                        .ok();
                }
            }
        }

        Ok(maybe_rx)
    }

    /// Maximum length accepted for user response text (defence against oversized payloads).
    const MAX_RESPONSE_LEN: usize = 8192;

    /// Route a user response back to the waiting task (if any).
    ///
    /// Validates that the notification exists and is a `Question` kind.  The
    /// double-response guard is enforced atomically by `UserInbox::set_response`
    /// (`UPDATE … WHERE response IS NULL`) so all concurrent callers are safe
    /// without an in-memory read-then-write race.
    pub async fn route_response(
        &self,
        notification_id: NotificationID,
        response: UserResponse,
    ) -> Result<(), AgentOSError> {
        // Reject oversized payloads before any database access.
        if response.text.len() > Self::MAX_RESPONSE_LEN {
            return Err(AgentOSError::KernelError {
                reason: format!(
                    "Response text exceeds maximum allowed length of {} characters",
                    Self::MAX_RESPONSE_LEN
                ),
            });
        }

        // Validate: must exist and be a Question.
        // (Kind never changes after creation — this check is race-free.)
        let msg =
            self.inbox
                .get(&notification_id)
                .await?
                .ok_or_else(|| AgentOSError::KernelError {
                    reason: format!("Notification {notification_id} not found"),
                })?;
        if !matches!(msg.kind, UserMessageKind::Question { .. }) {
            return Err(AgentOSError::KernelError {
                reason: format!("Notification {notification_id} is not a Question"),
            });
        }

        // Atomically persist — set_response returns an error if already responded.
        self.inbox.set_response(&notification_id, &response).await?;

        // Wake the waiting task (if it hasn't timed out yet).
        let mut map = self.waiting_tasks.write().await;
        if let Some(tx) = map.remove(&notification_id) {
            // Ignore send error — the task may have timed out and moved on.
            let _ = tx.send(response);
        }
        Ok(())
    }

    /// Remove a waiting-task entry without routing a response.
    ///
    /// Called when the `ask_user` safety timeout or kernel cancellation fires so
    /// dead `oneshot::Sender`s do not accumulate in the map between sweep cycles.
    pub async fn remove_waiting_task(&self, id: &NotificationID) {
        self.waiting_tasks.write().await.remove(id);
    }

    /// Return the notification IDs of all blocking questions currently awaiting a response.
    ///
    /// Used by `InboundRouter` to auto-route a free-text reply when exactly one task
    /// is blocked waiting for user input.
    pub async fn waiting_question_ids(&self) -> Vec<NotificationID> {
        self.waiting_tasks.read().await.keys().cloned().collect()
    }

    /// Sweep expired question messages: fire the `auto_action` for any blocking
    /// questions whose `expires_at` has passed and that still have a waiting sender.
    ///
    /// Called by the `TimeoutChecker` subsystem loop every 10 minutes.
    pub async fn sweep_expired_waiters(&self) {
        let now = Utc::now();
        let expired = self.inbox.list_expired_questions(now).await;
        let mut map = self.waiting_tasks.write().await;
        for msg in expired {
            if let Some(tx) = map.remove(&msg.id) {
                let auto_text = msg
                    .interaction
                    .as_ref()
                    .map(|i| i.auto_action.clone())
                    .unwrap_or_else(|| "<auto-denied>".to_string());
                let _ = tx.send(UserResponse {
                    text: auto_text.clone(),
                    responded_at: now,
                    channel: DeliveryChannel::cli(),
                });
                tracing::info!(
                    notification_id = %msg.id,
                    "Question notification timed out — auto-action fired"
                );
                let _ = self.audit.append(AuditEntry {
                    timestamp: now,
                    trace_id: TraceID::new(),
                    event_type: AuditEventType::NotificationAutoActioned,
                    agent_id: None,
                    task_id: msg.task_id,
                    tool_id: None,
                    details: serde_json::json!({
                        "notification_id": msg.id.to_string(),
                        "auto_action": auto_text,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
            }
        }
    }

    /// Return a clone of the `UserInbox` handle for use by command handlers.
    pub fn inbox(&self) -> Arc<UserInbox> {
        self.inbox.clone()
    }

    // ── Rate limiting ────────────────────────────────────────────────────────

    async fn check_rate_limit(&self, from: &NotificationSource) -> Result<(), AgentOSError> {
        let agent_id = match from {
            NotificationSource::Agent(id) => *id,
            // Kernel / System sources are not rate-limited.
            _ => return Ok(()),
        };
        let now = Utc::now();
        let mut limiter = self.rate_limiter.write().await;

        // Prune stale entries (window > 2 minutes old) when the map grows large,
        // preventing unbounded growth from many short-lived agents.
        const PRUNE_THRESHOLD: usize = 64;
        if limiter.len() > PRUNE_THRESHOLD {
            limiter.retain(|_, state| (now - state.window_start).num_seconds() < 120);
        }

        let state = limiter.entry(agent_id).or_insert(RateLimiterState {
            count: 0,
            window_start: now,
        });
        // Reset window if > 1 minute has elapsed.
        if (now - state.window_start).num_seconds() >= 60 {
            state.count = 0;
            state.window_start = now;
        }
        if state.count >= RATE_LIMIT_NOTIFY_PER_MIN {
            return Err(AgentOSError::RateLimited {
                detail: format!(
                    "max {} notifications per minute for agent {}",
                    RATE_LIMIT_NOTIFY_PER_MIN, agent_id
                ),
            });
        }
        state.count += 1;
        Ok(())
    }
}

// ── CLI Delivery Adapter ─────────────────────────────────────────────────────

/// The CLI delivery adapter.
///
/// Phase 1 model: all messages are already in the `UserInbox` SQLite DB.
/// The CLI reads from it via `agentctl notifications list`.  This adapter is
/// therefore a lightweight no-op for Phase 1 — it represents the CLI channel
/// in the delivery status map so future phases can badge an active TTY session.
pub struct CliDeliveryAdapter;

#[async_trait]
impl DeliveryAdapter for CliDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::cli()
    }

    async fn deliver(&self, _msg: &UserMessage) -> Result<(), DeliveryError> {
        // Phase 1: the message is already persisted in UserInbox.
        // An active TTY subscriber (Phase 2+) would push a badge here.
        Ok(())
    }

    async fn is_available(&self) -> bool {
        true
    }
}

// ── SSE (Web) Delivery Adapter ────────────────────────────────────────────────

/// Lightweight JSON payload sent over the SSE stream to connected browsers.
///
/// Kept small intentionally — the browser fetches the full message body via
/// HTMX when it receives this event.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationSsePayload {
    /// `NotificationID` as a hyphenated UUID string.
    pub id: String,
    pub subject: String,
    /// Lowercase priority string: "info" | "warning" | "urgent" | "critical".
    pub priority: String,
    /// Semantic category tag: "notification" | "question" | "task_complete" | "status_update".
    pub kind_tag: String,
    /// First 100 characters of the body.
    pub body_preview: String,
    /// `true` if the message expects a user reply.
    pub requires_response: bool,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// SSE delivery adapter — publishes `NotificationSsePayload` events to all
/// connected browser tabs via a `tokio::sync::broadcast` channel.
///
/// The channel sender is shared with `AppState` in `agentos-web` so the
/// web server can subscribe to it for `/notifications/stream`.
pub struct SseDeliveryAdapter {
    tx: broadcast::Sender<NotificationSsePayload>,
}

impl SseDeliveryAdapter {
    pub fn new(tx: broadcast::Sender<NotificationSsePayload>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl DeliveryAdapter for SseDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::web()
    }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        let payload = NotificationSsePayload {
            id: msg.id.to_string(),
            subject: msg.subject.clone(),
            priority: msg.priority.to_string().to_ascii_lowercase(),
            kind_tag: kind_to_tag(&msg.kind).to_string(),
            body_preview: msg.body.chars().take(100).collect(),
            requires_response: msg.interaction.is_some() && msg.response.is_none(),
            created_at: msg.created_at.to_rfc3339(),
        };
        // Ignore SendError — no active SSE subscribers is not an error.
        let _ = self.tx.send(payload);
        Ok(())
    }

    async fn is_available(&self) -> bool {
        // Always available so delivery status is correctly recorded even when
        // no browser tab is open; the broadcast message is simply discarded.
        true
    }
}

fn kind_to_tag(kind: &UserMessageKind) -> &'static str {
    match kind {
        UserMessageKind::Notification => "notification",
        UserMessageKind::Question { .. } => "question",
        UserMessageKind::TaskComplete { .. } => "task_complete",
        UserMessageKind::StatusUpdate { .. } => "status_update",
    }
}

pub fn parse_min_priority(s: &str) -> NotificationPriority {
    match s.to_ascii_lowercase().as_str() {
        "info" => NotificationPriority::Info,
        "urgent" => NotificationPriority::Urgent,
        "critical" => NotificationPriority::Critical,
        _ => NotificationPriority::Warning,
    }
}

// ── Webhook Delivery Adapter ──────────────────────────────────────────────────

/// Outbound HTTPS webhook adapter.
///
/// Posts a JSON payload to the configured URL on every delivered notification.
/// Supports HMAC-SHA256 request signing and configurable retry-with-backoff.
///
/// SSRF protection: the URL is validated at construction time via
/// `network_safety::validate_webhook_url`.
pub struct WebhookDeliveryAdapter {
    url: String,
    /// Pre-computed HMAC key bytes, or `None` if signing is disabled.
    hmac_key: Option<Vec<u8>>,
    min_priority: NotificationPriority,
    max_retries: u32,
    retry_delay: Duration,
    client: reqwest::Client,
}

/// JSON body sent to the webhook endpoint.
#[derive(Serialize)]
struct WebhookPayload<'a> {
    notification_id: &'a str,
    subject: &'a str,
    body: &'a str,
    priority: &'a str,
    kind_tag: &'a str,
    task_id: Option<String>,
    requires_response: bool,
    created_at: &'a str,
    agentos_version: &'static str,
}

impl WebhookDeliveryAdapter {
    /// Construct from config.  Returns an error if the URL fails SSRF validation
    /// or the `reqwest` client cannot be built.
    pub fn from_config(cfg: &WebhookAdapterConfig) -> Result<Self, AgentOSError> {
        crate::network_safety::validate_webhook_url(&cfg.url)?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("Failed to build webhook HTTP client: {e}"),
            })?;

        let hmac_key = if cfg.secret.is_empty() {
            None
        } else {
            Some(cfg.secret.as_bytes().to_vec())
        };

        Ok(Self {
            url: cfg.url.clone(),
            hmac_key,
            min_priority: parse_min_priority(&cfg.min_priority),
            max_retries: cfg.max_retries,
            retry_delay: Duration::from_secs(cfg.retry_delay_secs),
            client,
        })
    }
}

#[async_trait]
impl DeliveryAdapter for WebhookDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::webhook()
    }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        if msg.priority < self.min_priority {
            return Ok(());
        }

        let payload = WebhookPayload {
            notification_id: &msg.id.to_string(),
            subject: &msg.subject,
            body: &msg.body,
            priority: &msg.priority.to_string(),
            kind_tag: kind_to_tag(&msg.kind),
            task_id: msg.task_id.map(|id| id.to_string()),
            requires_response: msg.interaction.is_some(),
            created_at: &msg.created_at.to_rfc3339(),
            agentos_version: env!("CARGO_PKG_VERSION"),
        };

        let body_bytes = serde_json::to_vec(&payload).map_err(|e| DeliveryError(e.to_string()))?;

        let mut last_err = String::new();
        for attempt in 0..=self.max_retries {
            let mut req = self
                .client
                .post(&self.url)
                .header("Content-Type", "application/json")
                .header("X-AgentOS-Version", env!("CARGO_PKG_VERSION"));

            if let Some(key) = &self.hmac_key {
                use hmac::{Hmac, Mac};
                use sha2::Sha256;
                let mut mac =
                    Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts keys of any length");
                mac.update(&body_bytes);
                let sig = hex::encode(mac.finalize().into_bytes());
                req = req.header("X-AgentOS-Signature", format!("sha256={sig}"));
            }

            match req.body(body_bytes.clone()).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    last_err = format!("HTTP {}", resp.status());
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }

            if attempt < self.max_retries {
                // Exponential backoff: retry_delay * 2^attempt (capped at 60s)
                let backoff = self.retry_delay * 2u32.pow(attempt);
                let capped = backoff.min(Duration::from_secs(60));
                tokio::time::sleep(capped).await;
            }
        }

        Err(DeliveryError(format!(
            "Webhook delivery failed after {} attempts: {last_err}",
            self.max_retries + 1
        )))
    }

    async fn is_available(&self) -> bool {
        true
    }
}

// ── Desktop Delivery Adapter ──────────────────────────────────────────────────

/// Desktop notification adapter.
///
/// On Linux: uses `notify-send` (libnotify) via shell command — no additional
/// native dependency required.  Spawned as a non-blocking `tokio::process::Command`
/// so delivery never blocks the async runtime.
///
/// On non-Linux: always a no-op (not available).
pub struct DesktopDeliveryAdapter {
    min_priority: NotificationPriority,
    notify_on_task_complete: bool,
    /// Cached at construction: `true` if `notify-send` is on PATH (Linux only).
    available: bool,
}

impl DesktopDeliveryAdapter {
    pub fn new(min_priority: NotificationPriority, notify_on_task_complete: bool) -> Self {
        #[cfg(target_os = "linux")]
        let available = probe_notify_send();
        #[cfg(not(target_os = "linux"))]
        let available = false;

        Self {
            min_priority,
            notify_on_task_complete,
            available,
        }
    }
}

#[async_trait]
impl DeliveryAdapter for DesktopDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::custom(DeliveryChannel::DESKTOP)
    }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        // Always pass TaskComplete through when notify_on_task_complete is set,
        // regardless of priority threshold.
        let passes_priority = msg.priority >= self.min_priority;
        let is_task_complete = matches!(msg.kind, UserMessageKind::TaskComplete { .. });
        if !(passes_priority || (is_task_complete && self.notify_on_task_complete)) {
            return Ok(());
        }

        #[cfg(target_os = "linux")]
        {
            let urgency = match msg.priority {
                NotificationPriority::Critical => "critical",
                NotificationPriority::Urgent => "normal",
                _ => "low",
            };
            let subject = msg.subject.clone();
            let body_preview: String = msg.body.chars().take(150).collect();
            // Fire-and-forget: spawn notify-send and ignore errors so a missing
            // notification daemon never causes delivery failures.
            let _ = tokio::process::Command::new("notify-send")
                .arg("--urgency")
                .arg(urgency)
                .arg("--expire-time")
                .arg("8000")
                .arg("--app-name")
                .arg("AgentOS")
                .arg(&subject)
                .arg(&body_preview)
                .spawn();
        }

        Ok(())
    }

    async fn is_available(&self) -> bool {
        self.available
    }
}

/// Probes whether `notify-send` is available at adapter construction time.
/// Result is cached in `DesktopDeliveryAdapter::available`.
///
/// Checks well-known installation paths via `stat` (a single syscall per path)
/// instead of spawning a child process, so this never blocks the async runtime.
#[cfg(target_os = "linux")]
fn probe_notify_send() -> bool {
    const KNOWN_PATHS: &[&str] = &[
        "/usr/bin/notify-send",
        "/usr/local/bin/notify-send",
        "/usr/local/sbin/notify-send",
        "/opt/local/bin/notify-send",
    ];
    KNOWN_PATHS.iter().any(|p| std::path::Path::new(p).exists())
}

// ── Slack Delivery Adapter ────────────────────────────────────────────────────

/// Slack incoming-webhook adapter.
///
/// Posts a Block Kit message to the configured Slack webhook URL when a
/// `UserMessage` meets the minimum priority threshold.
pub struct SlackDeliveryAdapter {
    webhook_url: String,
    min_priority: NotificationPriority,
    include_body: bool,
    max_retries: u32,
    retry_delay: Duration,
    client: reqwest::Client,
}

impl SlackDeliveryAdapter {
    /// Construct from config.  Returns an error if the URL fails SSRF validation.
    pub fn from_config(cfg: &SlackAdapterConfig) -> Result<Self, AgentOSError> {
        crate::network_safety::validate_webhook_url(&cfg.webhook_url)?;
        Ok(Self {
            webhook_url: cfg.webhook_url.clone(),
            min_priority: parse_min_priority(&cfg.min_priority),
            include_body: cfg.include_body,
            max_retries: cfg.max_retries,
            retry_delay: Duration::from_secs(cfg.retry_delay_secs),
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl DeliveryAdapter for SlackDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::custom(DeliveryChannel::SLACK)
    }

    async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
        if msg.priority < self.min_priority {
            return Ok(());
        }

        let emoji = match msg.priority {
            NotificationPriority::Critical => ":rotating_light:",
            NotificationPriority::Urgent => ":warning:",
            NotificationPriority::Warning => ":large_yellow_circle:",
            NotificationPriority::Info => ":information_source:",
        };

        let header_text = format!("{emoji} *AgentOS* — {}", msg.subject);

        let mut blocks = vec![serde_json::json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": header_text }
        })];

        if self.include_body && !msg.body.is_empty() {
            let body_text: String = msg.body.chars().take(500).collect();
            blocks.push(serde_json::json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": body_text }
            }));
        }

        if msg.interaction.is_some() {
            blocks.push(serde_json::json!({
                "type": "context",
                "elements": [{
                    "type": "mrkdwn",
                    "text": "_Reply via `agentctl notifications respond` or the web UI._"
                }]
            }));
        }

        let payload = serde_json::json!({ "blocks": blocks });

        let mut last_err = String::new();
        for attempt in 0..=self.max_retries {
            match self
                .client
                .post(&self.webhook_url)
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    last_err = format!("HTTP {}", resp.status());
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }

            if attempt < self.max_retries {
                // Exponential backoff: retry_delay * 2^attempt (capped at 60s)
                let backoff = self.retry_delay * 2u32.pow(attempt);
                let capped = backoff.min(Duration::from_secs(60));
                tokio::time::sleep(capped).await;
            }
        }

        Err(DeliveryError(format!(
            "Slack delivery failed after {} attempts: {last_err}",
            self.max_retries + 1
        )))
    }

    async fn is_available(&self) -> bool {
        true
    }
}
