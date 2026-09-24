use crate::config::{SlackAdapterConfig, WebhookAdapterConfig};
use crate::user_inbox::UserInbox;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_channels::manager::ChannelManager;
use agentos_channels::types::{MessageContent, OutboundMessage};
use agentos_types::{
    AgentID, AgentOSError, AttachmentKind, ChannelInstanceID, DeliveryChannel, DeliveryStatus,
    NotificationID, NotificationPriority, NotificationSource, TraceID, UserMessage,
    UserMessageKind, UserResponse,
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
    /// Stored inbound image attachments as `(file_id, mime)`, populated by the
    /// InboundRouter after download. Carried into the chat context as
    /// `ContentPart::Image::FileRef` so vision-capable agents can see them.
    /// Empty for non-image media and channels without media support.
    pub media_file_ids: Vec<(String, String)>,
    /// Remote media URLs extracted from a non-Telegram channel's inbound content
    /// (e.g. Discord CDN attachments), to be downloaded + stored by the
    /// InboundRouter under an SSRF guard. Empty for Telegram (which downloads via
    /// `getFile`) and channels whose adapters don't yet emit inbound media.
    pub pending_media: Vec<InboundMediaUrl>,
}

/// A remote media URL awaiting download + storage by the InboundRouter.
#[derive(Debug, Clone)]
pub struct InboundMediaUrl {
    pub url: String,
    /// Original filename, if the platform provided one.
    pub filename: Option<String>,
    /// Platform-declared MIME, if any (the downloader sniffs/falls back otherwise).
    pub mime: Option<String>,
}

/// Pluggable delivery channel adapter.
///
/// Each adapter handles one delivery channel (CLI, Web SSE, Webhook, …).
/// The `NotificationRouter` calls `deliver` on every available adapter after
/// writing the message to the `UserInbox`.
///
/// Adapters that support receiving inbound messages from the user implement
/// `supports_inbound() → true` and `start_listening(tx)`.
/// How long a cosmetic "typing…" ping may take before it is abandoned.
///
/// Deliberately shorter than the ~5s an indicator lives for: a ping still in
/// flight when the indicator has already expired cannot refresh anything, and
/// waiting on it only delays the next attempt.
const TYPING_PING_TIMEOUT: Duration = Duration::from_secs(2);

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

    /// Show the channel's native "typing…" indicator, if it has one.
    ///
    /// Purely cosmetic: it tells the user an agent is working on a turn that may
    /// take minutes. Defaults to a no-op so an adapter without the concept — and
    /// every adapter that predates this method — opts out by doing nothing.
    ///
    /// Indicators expire on their own (Telegram after ~5s), so this is called
    /// repeatedly for the life of a turn and needs no matching "stop" call.
    /// Implementations must be cheap and must never block on a rate-limit
    /// backoff: a dropped ping costs nothing, a stalled one delays the reply.
    async fn typing(&self) -> Result<(), DeliveryError> {
        Ok(())
    }

    /// Whether this adapter delivers to a one-to-one chat with the operator,
    /// so it may receive the full approval request (task text, target,
    /// redacted payload). Defaults to `false`: a push service, webhook,
    /// desktop banner or group chat gets only the summary.
    async fn is_private_chat(&self) -> bool {
        false
    }

    /// Strip the interactive controls from every message this adapter sent
    /// under `thread_id`, because the decision they offered was already made
    /// (possibly on another surface). Defaults to a no-op; a late tap on a
    /// control left in place is still answered "already resolved".
    async fn retract_actions(&self, _thread_id: &str) -> Result<(), DeliveryError> {
        Ok(())
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

    /// When the outbound recipient was unknown at connect time (Telegram webhook +
    /// empty `chat_id`), apply the discovered ID on the matching adapter instance.
    ///
    /// Long-poll Telegram instead discovers inside `telegram_poll_loop`; webhook-only
    /// adapters skip that loop, so the kernel calls this from `InboundRouter` on the
    /// first inbound message.
    async fn hydrate_discovered_recipient(
        &self,
        _channel_instance_id: &ChannelInstanceID,
        _external_id: &str,
    ) -> bool {
        false
    }
}

/// Kernel subsystem that receives `UserMessage` objects from agents or kernel
/// internals and dispatches them to all registered delivery adapters while
/// persisting every message in the `UserInbox`.
///
/// This is the single authoritative dispatcher — delivery adapters are leaf
/// nodes that have no knowledge of each other.
///
/// The routing-matrix column for an adapter: its instance id for registered
/// channels (`telegram-main`), else its builtin kind (`desktop`, `cli`, `web`,
/// `webhook`, `slack`).
fn channel_key(adapter: &Arc<dyn DeliveryAdapter>) -> String {
    adapter
        .adapter_instance_id()
        .unwrap_or_else(|| adapter.channel_id().as_str().to_string())
}

pub struct NotificationRouter {
    inbox: Arc<UserInbox>,
    audit: Arc<agentos_audit::AuditLog>,
    adapters: RwLock<Vec<Arc<dyn DeliveryAdapter>>>,
    /// Pending oneshot senders for blocking `ask_user` questions.
    /// Key: the `NotificationID` of the Question message.
    waiting_tasks: Arc<RwLock<HashMap<NotificationID, oneshot::Sender<UserResponse>>>>,
    /// Per-agent rate limiter state.
    rate_limiter: Arc<RwLock<HashMap<AgentID, RateLimiterState>>>,
    /// Handle to the *other* outbound stack — the `agentos-channels`
    /// `ChannelManager`, which owns Discord/Slack/WhatsApp/Webhook while this
    /// router owns Telegram/Ntfy/Email (see `Kernel::build_channel_adapter`).
    ///
    /// Attached lazily rather than passed to `new`: the kernel builds the
    /// router before the manager exists. Set on the first channel connect /
    /// restore, which is the earliest point any send can happen.
    channel_manager: std::sync::OnceLock<Arc<ChannelManager>>,
    /// Operator routing matrix: which event kinds reach which channels.
    ///
    /// Attached after construction (the matrix needs the kernel state store,
    /// which is built later), exactly as `channel_manager` is. A router with
    /// no matrix attached — every unit test that builds a bare router —
    /// delivers everything, so the gate fails open.
    routes: std::sync::OnceLock<Arc<crate::notification_routes::RouteMatrix>>,
}

impl NotificationRouter {
    pub fn new(inbox: Arc<UserInbox>, audit: Arc<agentos_audit::AuditLog>) -> Self {
        Self {
            inbox,
            audit,
            adapters: RwLock::new(vec![Arc::new(CliDeliveryAdapter)]),
            waiting_tasks: Arc::new(RwLock::new(HashMap::new())),
            rate_limiter: Arc::new(RwLock::new(HashMap::new())),
            channel_manager: std::sync::OnceLock::new(),
            routes: std::sync::OnceLock::new(),
        }
    }

    /// Give the router a handle to the `ChannelManager` outbound stack so
    /// [`send_to_channel`](Self::send_to_channel) can reach manager-owned
    /// kinds. Idempotent; later calls are ignored.
    pub fn attach_channel_manager(&self, manager: Arc<ChannelManager>) {
        let _ = self.channel_manager.set(manager);
    }

    /// Give the router the operator's notification routing matrix. Idempotent;
    /// later calls are ignored.
    pub fn attach_routes(&self, routes: Arc<crate::notification_routes::RouteMatrix>) {
        let _ = self.routes.set(routes);
    }

    /// The attached routing matrix, if any. `ChannelBroadcastSink` reads it
    /// through here rather than holding a second handle.
    pub fn routes(&self) -> Option<Arc<crate::notification_routes::RouteMatrix>> {
        self.routes.get().cloned()
    }

    /// Whether the matrix permits this event on this channel. Fails open when
    /// no matrix is attached.
    fn route_allows(&self, event: agentos_types::NotificationEvent, channel_key: &str) -> bool {
        match self.routes.get() {
            Some(routes) => routes.allows(event, channel_key),
            None => true,
        }
    }

    /// Record a matrix-suppressed delivery: `Skipped` on the inbox row plus an
    /// audit entry, so "why didn't I get that?" is answerable after the fact.
    async fn mark_suppressed(
        &self,
        msg: &UserMessage,
        channel: DeliveryChannel,
        event: agentos_types::NotificationEvent,
        channel_key: &str,
    ) {
        let mode = self
            .routes
            .get()
            .map(|r| r.mode(event, channel_key).to_string())
            .unwrap_or_default();
        self.inbox
            .update_delivery_status(&msg.id, channel.clone(), DeliveryStatus::Skipped)
            .await
            .ok();
        let _ = self.audit.append(AuditEntry {
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::NotificationSuppressed,
            agent_id: None,
            task_id: msg.task_id,
            tool_id: None,
            details: serde_json::json!({
                "notification_id": msg.id.to_string(),
                "channel": channel.to_string(),
                "event": event.as_str(),
                "mode": mode,
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });
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

    /// The `adapter_instance_id` of every registered adapter.
    ///
    /// Used by the escalation sink to avoid sending the same approval prompt
    /// twice: a connected channel is registered here as a delivery adapter
    /// (`cmd_connect_channel`), so `deliver` already reaches it, and the sink's
    /// paired-DM loop must skip senders on that channel.
    /// Every delivery target this router can reach, as
    /// `(matrix key, channel kind, available now)` — the column axis of the
    /// notification routing matrix.
    pub async fn adapter_targets(&self) -> Vec<(String, String, bool)> {
        let adapters = self.adapters.read().await;
        let mut out = Vec::with_capacity(adapters.len());
        for adapter in adapters.iter() {
            out.push((
                channel_key(adapter),
                adapter.channel_id().as_str().to_string(),
                adapter.is_available().await,
            ));
        }
        out
    }

    pub async fn adapter_instance_ids(&self) -> std::collections::HashSet<String> {
        self.adapters
            .read()
            .await
            .iter()
            .filter_map(|a| a.adapter_instance_id())
            .collect()
    }

    /// Notify delivery adapters that a channel instance now has a concrete external
    /// recipient id (e.g. Telegram `chat_id` learned from the first webhook update).
    pub async fn hydrate_discovered_recipient(
        &self,
        channel_instance_id: &ChannelInstanceID,
        external_id: &str,
    ) {
        if external_id.is_empty() {
            return;
        }
        let adapters = self.adapters.read().await;
        for adapter in adapters.iter() {
            if adapter
                .hydrate_discovered_recipient(channel_instance_id, external_id)
                .await
            {
                tracing::info!(
                    %channel_instance_id,
                    %external_id,
                    "Hydrated delivery adapter with discovered external recipient"
                );
                return;
            }
        }
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

        // Fan out to all available adapters (best-effort; failures are logged),
        // minus the channels the operator's routing matrix mutes for this
        // event kind.
        let event = agentos_types::NotificationEvent::classify(&msg);
        let adapters = self.adapters.read().await;
        for adapter in adapters.iter() {
            let key = channel_key(adapter);
            if !self.route_allows(event, &key) {
                self.mark_suppressed(&msg, adapter.channel_id(), event, &key)
                    .await;
                continue;
            }
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

    /// Like `deliver`, but only fans out to adapters whose `adapter_instance_id`
    /// is in `instance_ids` OR whose `channel_id` is in `channel_kinds`.
    ///
    /// Used by `notify-user` when the agent picks one or more specific channels.
    /// Inbox persistence and rate-limit checks still run; non-matching adapters
    /// are recorded as `Skipped` so the inbox view shows why each channel was
    /// (or wasn't) used.
    pub async fn deliver_filtered(
        &self,
        msg: UserMessage,
        instance_ids: &std::collections::HashSet<String>,
        channel_kinds: &std::collections::HashSet<String>,
    ) -> Result<(), AgentOSError> {
        self.check_rate_limit(&msg.from).await?;
        self.inbox.write(&msg).await?;

        let event = agentos_types::NotificationEvent::classify(&msg);
        let adapters = self.adapters.read().await;
        for adapter in adapters.iter() {
            let inst = adapter.adapter_instance_id();
            let kind = adapter.channel_id().as_str().to_string();
            let selected = inst
                .as_ref()
                .map(|i| instance_ids.contains(i))
                .unwrap_or(false)
                || channel_kinds.contains(&kind);
            let key = channel_key(adapter);
            if selected && !self.route_allows(event, &key) {
                self.mark_suppressed(&msg, adapter.channel_id(), event, &key)
                    .await;
                continue;
            }
            if !selected {
                self.inbox
                    .update_delivery_status(&msg.id, adapter.channel_id(), DeliveryStatus::Skipped)
                    .await
                    .ok();
                continue;
            }
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
                            "filtered": true,
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
                        "Filtered notification delivery failed on channel"
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
        Ok(())
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

        // An expired question's asker has already taken its auto_action; storing
        // a late answer would report "sent" for a reply no agent will read.
        if msg.expires_at.is_some_and(|at| at < Utc::now()) {
            return Err(AgentOSError::KernelError {
                reason: format!("Question {notification_id} has expired"),
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

    /// Drop an `ask-user` waiter that timed out on the asker's side, with the
    /// same audit row the periodic sweep writes.
    pub async fn expire_waiter(
        &self,
        id: &NotificationID,
        task_id: Option<agentos_types::TaskID>,
        auto_action: &str,
    ) {
        if self.waiting_tasks.write().await.remove(id).is_none() {
            return;
        }
        let _ = self.audit.append(AuditEntry {
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::NotificationAutoActioned,
            agent_id: None,
            task_id,
            tool_id: None,
            details: serde_json::json!({
                "notification_id": id.to_string(),
                "auto_action": auto_action,
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });
    }

    /// Drop every waiter belonging to `task_id`. Called on task cancel: the
    /// asker wakes with its fallback and aborts on the terminal state, and the
    /// question stops counting as open for inbound channel replies.
    pub async fn drop_waiters_for_task(&self, task_id: &agentos_types::TaskID) {
        let ids: Vec<NotificationID> = self.waiting_tasks.read().await.keys().cloned().collect();
        for id in ids {
            let owned =
                matches!(self.inbox.get(&id).await, Ok(Some(msg)) if msg.task_id == Some(*task_id));
            if owned {
                self.waiting_tasks.write().await.remove(&id);
            }
        }
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

    /// Deliver a message to a single channel adapter identified by `target_instance_id`.
    ///
    /// Unlike `deliver`, this does not fan out to all registered adapters and does not
    /// persist to the user inbox — channel-bound traffic (agent→Telegram, kernel `/help`
    /// replies, agent chat via channel) must not pollute the user-facing notifications
    /// page (which is reserved for `notify-user`-style messages). Each caller is
    /// responsible for emitting a `ChannelMessageSent` audit entry on success
    /// (`kernel_action::execute_channel_send`, `InboundRouter::send_reply`,
    /// `InboundRouter::send_agent_chat_reply`). Used by `InboundRouter` and `channel-send`
    /// to route replies back to the originating channel only, preventing cross-channel
    /// leakage of private chat content.
    pub async fn deliver_to_channel(
        &self,
        msg: UserMessage,
        target_instance_id: &str,
    ) -> Result<(), AgentOSError> {
        self.check_rate_limit(&msg.from).await?;
        let Some(adapter) = self.adapter_for(target_instance_id).await else {
            // Was `debug!`: a reply dropped here costs an LLM turn and the user
            // sees nothing, so it must be visible at the default log level.
            tracing::warn!(
                target = %target_instance_id,
                notification_id = %msg.id,
                "deliver_to_channel: no DeliveryAdapter for this channel instance — \
                 message dropped (a ChannelManager-owned kind such as Discord/Slack/\
                 WhatsApp/Webhook must go through send_to_channel)"
            );
            return Ok(());
        };
        Self::deliver_via(&adapter, msg, target_instance_id).await
    }

    /// Deliver to an **already-resolved** adapter.
    ///
    /// Split out so `send_to_channel` does not re-resolve what it just looked
    /// up: besides the second read-lock per send, the gap between the two
    /// lookups was a TOCTOU window in which a concurrent `deregister_adapter`
    /// turned a would-be `ChannelManager` retry into a silent `Ok(())`.
    async fn deliver_via(
        adapter: &Arc<dyn DeliveryAdapter>,
        msg: UserMessage,
        target_instance_id: &str,
    ) -> Result<(), AgentOSError> {
        // Unavailable is a failure, not a silent success. Falling through to
        // `Ok(())` made `channel-send` answer `{"status":"delivered"}` and
        // write a `ChannelMessageSent` audit row for a message nothing sent —
        // permanently for the Email stub (`is_available` is hardcoded false),
        // and for Telegram whenever `chat_id` has not been discovered yet.
        if !adapter.is_available().await {
            let reason = format!(
                "channel '{target_instance_id}' ({}) reported itself unavailable — nothing was \
                 sent (unconfigured credentials, an adapter that is not implemented, or a \
                 recipient not yet discovered: send the bot a message first)",
                adapter.channel_id()
            );
            tracing::warn!(
                notification_id = %msg.id,
                target = %target_instance_id,
                reason = %reason,
                "Targeted channel delivery refused: adapter unavailable"
            );
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "channel-delivery".to_string(),
                reason,
            });
        }
        // Propagate delivery failure so callers (e.g. channel-send)
        // report it instead of falsely claiming success — important
        // for media sends where sendPhoto/sendDocument can 400.
        if let Err(e) = adapter.deliver(&msg).await {
            tracing::warn!(
                notification_id = %msg.id,
                target = %target_instance_id,
                error = %e,
                "Targeted channel delivery failed"
            );
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "channel-delivery".to_string(),
                reason: e.to_string(),
            });
        }
        Ok(())
    }

    /// Send one message to a single channel instance on **whichever outbound
    /// stack owns it** — the single entry point every channel-bound sender
    /// should use.
    ///
    /// The kernel has two stacks and they are disjoint: `DeliveryAdapter`s
    /// registered on this router (Telegram, Ntfy, Email) and `ChannelAdapter`s
    /// registered with the `agentos-channels` [`ChannelManager`] (Discord,
    /// Slack, WhatsApp, Webhook) — the split is decided in
    /// `Kernel::build_channel_adapter`. Callers that picked a stack themselves
    /// were each blind to half the connected channels (replies never reached
    /// Discord; escalation prompts never reached Telegram), so routing is
    /// resolved here from where the instance is actually registered rather
    /// than from a `ChannelKind` match that every caller had to repeat.
    pub async fn send_to_channel(
        &self,
        msg: UserMessage,
        target_instance_id: &str,
    ) -> Result<(), AgentOSError> {
        if let Some(adapter) = self.adapter_for(target_instance_id).await {
            self.check_rate_limit(&msg.from).await?;
            return Self::deliver_via(&adapter, msg, target_instance_id).await;
        }
        let Some(manager) = self.channel_manager.get() else {
            tracing::warn!(
                target = %target_instance_id,
                "send_to_channel: no DeliveryAdapter for this channel instance and no \
                 ChannelManager attached — message dropped"
            );
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "channel-delivery".to_string(),
                reason: format!("no outbound adapter for channel '{target_instance_id}'"),
            });
        };
        self.check_rate_limit(&msg.from).await?;
        manager
            .send(target_instance_id, outbound_from(&msg, target_instance_id))
            .await
    }

    /// Split registered adapters for an escalation fan-out:
    /// `(private chat instance ids, filter sets for everything else)`. The
    /// second part is shaped for [`Self::deliver_filtered`] — instance ids for
    /// instance-registered adapters, kinds for built-ins that have none.
    pub async fn split_private_chats(
        &self,
    ) -> (
        std::collections::HashSet<String>,
        (
            std::collections::HashSet<String>,
            std::collections::HashSet<String>,
        ),
    ) {
        let adapters = self.adapters.read().await.clone();
        let mut private = std::collections::HashSet::new();
        let mut others = std::collections::HashSet::new();
        let mut kinds = std::collections::HashSet::new();
        for adapter in adapters {
            match adapter.adapter_instance_id() {
                Some(id) if adapter.is_private_chat().await => {
                    private.insert(id);
                }
                Some(id) => {
                    others.insert(id);
                }
                None => {
                    kinds.insert(adapter.channel_id().as_str().to_string());
                }
            }
        }
        (private, (others, kinds))
    }

    /// Ask every adapter to strip the controls it sent under `thread_id`.
    ///
    /// Best-effort and bounded per adapter: the decision is already recorded,
    /// so a failure here only leaves a stale button whose tap is answered
    /// "already resolved".
    pub async fn retract_actions(&self, thread_id: &str) {
        const RETRACT_TIMEOUT: Duration = Duration::from_secs(10);
        let adapters = self.adapters.read().await.clone();
        for adapter in adapters {
            match tokio::time::timeout(RETRACT_TIMEOUT, adapter.retract_actions(thread_id)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::debug!(thread_id, error = %e, "retract_actions failed"),
                Err(_) => tracing::debug!(thread_id, "retract_actions timed out"),
            }
        }
    }

    /// The `DeliveryAdapter` registered for `instance_id`, if this router owns it.
    async fn adapter_for(&self, instance_id: &str) -> Option<Arc<dyn DeliveryAdapter>> {
        self.adapters
            .read()
            .await
            .iter()
            .find(|a| a.adapter_instance_id().as_deref() == Some(instance_id))
            .cloned()
    }

    /// Ping the "typing…" indicator on `instance_id`'s channel, if it has one.
    ///
    /// Returns `false` when the keepalive driving this should give up for the
    /// rest of the turn — no adapter owns the channel, or the ping failed.
    /// Nothing here is ever fatal to a turn: the indicator is cosmetic and every
    /// outcome is logged at debug, because this runs on a timer and a warn would
    /// flood the log for a miss nobody can see.
    ///
    /// Unlike [`Self::send_to_channel`] this has **no `ChannelManager`
    /// fallback**: it reaches only adapters this router owns. Manager-stack
    /// kinds (Discord, Slack, WhatsApp, Webhook…) are therefore silently
    /// indicator-free until the indicator is plumbed through that stack too.
    pub async fn typing_on_channel(&self, instance_id: &str) -> bool {
        // A turn parked on a blocking question or an approval is not working, it
        // is waiting for the person reading the channel. Claiming "typing…" there
        // reads as "sit tight, an answer is coming" and talks them out of the one
        // action that can unblock it.
        //
        // ponytail: suppresses on *any* outstanding blocking interaction rather
        // than one scoped to this channel, because `waiting_tasks` is keyed by
        // NotificationID and holds no channel. `InboundRouter::run` serializes
        // turns per channel only, so with two channels mid-turn one parked turn
        // mutes the other's indicator. Cosmetic; key this by channel if
        // multi-channel operators notice.
        if !self.waiting_tasks.read().await.is_empty() {
            return true;
        }

        let Some(adapter) = self.adapter_for(instance_id).await else {
            tracing::trace!(instance_id, "No delivery adapter owns this channel");
            return false;
        };

        // A ping that outlives the indicator it is refreshing is worse than no
        // ping: it blanks the signal and stalls the keepalive behind it. The
        // adapter's own HTTP client can allow far longer than that, so bound it
        // here, once, for every adapter rather than in each impl.
        match tokio::time::timeout(TYPING_PING_TIMEOUT, adapter.typing()).await {
            Ok(Ok(())) => true,
            // Stop rather than retry. A ping fails for reasons that do not heal
            // within a turn — revoked token, blocked bot, flood control — and
            // re-firing every 4s for the remaining 600s of a turn turns a
            // cosmetic miss into ~165 futile requests, which for flood control
            // actively lengthens the ban on the token that also delivers replies.
            Ok(Err(e)) => {
                tracing::debug!(instance_id, error = %e, "Typing indicator ping failed");
                false
            }
            Err(_) => {
                tracing::debug!(instance_id, "Typing indicator ping timed out");
                false
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

/// Project a `UserMessage` onto the `ChannelManager` wire type.
///
/// `UserMessage::thread_id` is the kernel's own conversation key
/// (`"channel:<uuid>"`); the *platform* thread is `reply_to_external_id`, which
/// is what an adapter must reply into.
fn outbound_from(msg: &UserMessage, instance_id: &str) -> OutboundMessage {
    // The delivery-stack adapters render "<subject>\n\n<body>". Mirror that,
    // except when the subject is just a truncated copy of the body (how
    // `InboundRouter` fills it) — repeating it reads as a bug.
    let text = if msg.subject.is_empty() || msg.body.starts_with(&msg.subject) {
        msg.body.clone()
    } else {
        format!("**{}**\n\n{}", msg.subject, msg.body)
    };

    let content = match &msg.attachment {
        // ponytail: `attachment.inline` (base64 upload) is Telegram-only, i.e.
        // delivery-stack-only; manager-stack adapters get the URL form.
        Some(att) => {
            let media = match att.kind {
                AttachmentKind::Image => MessageContent::Image {
                    url: att.url.clone(),
                    alt: att.caption.clone(),
                },
                AttachmentKind::Document => MessageContent::File {
                    url: att.url.clone(),
                    filename: att.filename.clone().unwrap_or_else(|| "file".into()),
                    mime: String::new(),
                },
            };
            let mut parts = Vec::new();
            if !text.is_empty() {
                parts.push(MessageContent::Text(text));
            }
            parts.push(media);
            // No native album off Telegram — emit each extra URL as its own part.
            for extra in &att.group_urls {
                parts.push(MessageContent::Image {
                    url: extra.clone(),
                    alt: None,
                });
            }
            if parts.len() == 1 {
                parts.remove(0)
            } else {
                MessageContent::Mixed(parts)
            }
        }
        // `Text`, not `Markdown`: every manager-stack adapter renders the two
        // arms identically (`render_for_delivery`/`as_text`/`text_caption`),
        // except the Webhook adapter, which serializes `content` verbatim into
        // an HMAC-signed POST body. `MessageContent` is
        // `#[serde(tag = "type", content = "data")]`, so `Markdown` silently
        // changed that published contract to `{"type":"Markdown",…}`.
        None => MessageContent::Text(text),
    };

    OutboundMessage {
        actions: msg.actions.clone(),
        channel_instance_id: instance_id.to_string(),
        content,
        thread_id: msg.reply_to_external_id.clone(),
    }
}

// ── CLI Delivery Adapter ─────────────────────────────────────────────────────

/// The CLI delivery adapter.
///
/// Phase 1 model: all messages are already in the `UserInbox` SQLite DB.
/// The CLI reads from it via `agentos notifications list`.  This adapter is
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
/// HTTP surfaces subscribe to the channel sender to stream notifications.
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
    /// Actionable controls, omitted entirely when there are none so existing
    /// consumers see an unchanged payload shape.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    actions: &'a [agentos_types::PromptAction],
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
            // Structural rather than baked into `body`: a webhook consumer is
            // usually a script, and these are the literal commands it POSTs
            // back at the inbound webhook to resolve the escalation.
            actions: &msg.actions,
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
            // Text fallback, not Block Kit buttons: a Slack button POSTs to an
            // Interactivity Request URL that AgentOS does not expose yet, so a
            // rendered button would be tapped and go nowhere while the
            // escalation aged into auto-deny. See phase 08 of the
            // approval-channel-fanout plan.
            // Truncate the body, THEN append: truncating the concatenation
            // could cut mid-command, and `/approve 4` resolves a different
            // escalation than the `/approve 42` the operator was shown.
            let truncated: String = msg.body.chars().take(500).collect();
            let body_text = format!(
                "{truncated}{}",
                agentos_types::render_actions_fallback(&msg.actions)
            );
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
                    "text": "_Reply via `agentos notifications respond` or the web UI._"
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for a Telegram/Ntfy `DeliveryAdapter`: records bodies instead
    /// of hitting the network.
    struct RecordingAdapter {
        instance_id: String,
        seen: Arc<RwLock<Vec<String>>>,
        /// Mirrors Telegram before `chat_id` discovery / the Email stub.
        available: bool,
        /// Counts `typing()` calls so the indicator can be asserted on.
        typings: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl RecordingAdapter {
        fn new(instance_id: &str, seen: Arc<RwLock<Vec<String>>>) -> Self {
            Self {
                instance_id: instance_id.to_string(),
                seen,
                available: true,
                typings: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn typing_count(&self) -> usize {
            self.typings.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl DeliveryAdapter for RecordingAdapter {
        fn channel_id(&self) -> DeliveryChannel {
            DeliveryChannel::custom("telegram".to_string())
        }
        async fn deliver(&self, msg: &UserMessage) -> Result<(), DeliveryError> {
            self.seen.write().await.push(msg.body.clone());
            Ok(())
        }
        async fn is_available(&self) -> bool {
            self.available
        }
        fn adapter_instance_id(&self) -> Option<String> {
            Some(self.instance_id.clone())
        }
        async fn typing(&self) -> Result<(), DeliveryError> {
            self.typings
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    fn kernel_msg(body: &str) -> UserMessage {
        UserMessage {
            actions: Vec::new(),
            id: NotificationID::new(),
            from: NotificationSource::Kernel,
            task_id: None,
            trace_id: TraceID::new(),
            kind: UserMessageKind::Notification,
            priority: NotificationPriority::Info,
            subject: body.chars().take(80).collect(),
            body: body.to_string(),
            interaction: None,
            delivery_status: HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: Some("channel:abc".to_string()),
            reply_to_external_id: None,
            attachment: None,
        }
    }

    fn test_router() -> Arc<NotificationRouter> {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = Arc::new(
            crate::user_inbox::UserInbox::new(&dir.path().join("inbox.db"), 100)
                .expect("open user inbox"),
        );
        let audit = Arc::new(
            agentos_audit::AuditLog::open(&dir.path().join("audit.db")).expect("open audit log"),
        );
        // Keep the SQLite files alive for the duration of the test.
        Box::leak(Box::new(dir));
        Arc::new(NotificationRouter::new(inbox, audit))
    }

    fn empty_channel_manager() -> Arc<ChannelManager> {
        let (tx, rx) = mpsc::channel(1);
        // Hold the receiver open so `send` failures are "not found", not "closed".
        Box::leak(Box::new(rx));
        Arc::new(ChannelManager::new(
            tx,
            tokio_util::sync::CancellationToken::new(),
        ))
    }

    /// Attach a real `RouteMatrix` backed by a temp state DB.
    async fn attach_test_routes(
        router: &Arc<NotificationRouter>,
    ) -> (
        Arc<crate::notification_routes::RouteMatrix>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            crate::state_store::KernelStateStore::open(dir.path().join("state.db"))
                .await
                .expect("open state store"),
        );
        Box::leak(Box::new(dir));
        let panel = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let routes = Arc::new(
            crate::notification_routes::RouteMatrix::load(store, Arc::clone(&panel))
                .await
                .expect("load matrix"),
        );
        router.attach_routes(Arc::clone(&routes));
        (routes, panel)
    }

    #[tokio::test]
    async fn router_without_routes_delivers_everything() {
        // Fail-open guard: a bare router (every pre-existing unit test) must
        // keep delivering when no matrix has been attached.
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(RecordingAdapter::new(
                "telegram-1",
                Arc::clone(&seen),
            )))
            .await;
        router
            .deliver(kernel_msg("ungated"))
            .await
            .expect("deliver");
        assert_eq!(seen.read().await.len(), 1);
    }

    #[tokio::test]
    async fn never_rule_suppresses_only_that_channel() {
        let router = test_router();
        let muted = Arc::new(RwLock::new(Vec::new()));
        let open = Arc::new(RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(RecordingAdapter::new(
                "telegram-1",
                Arc::clone(&muted),
            )))
            .await;
        router
            .register_adapter(Arc::new(RecordingAdapter::new(
                "telegram-2",
                Arc::clone(&open),
            )))
            .await;
        let (routes, _panel) = attach_test_routes(&router).await;
        routes
            .set(
                agentos_types::NotificationEvent::SystemAlert,
                "telegram-1",
                crate::notification_routes::RouteMode::Never,
            )
            .await
            .expect("set rule");

        router.deliver(kernel_msg("alert")).await.expect("deliver");

        assert!(
            muted.read().await.is_empty(),
            "muted channel must see nothing"
        );
        assert_eq!(open.read().await.len(), 1, "other channel still delivers");
    }

    #[tokio::test]
    async fn suppressed_message_is_still_written_to_the_inbox() {
        // Muting a channel must never lose the record — the panel bell and
        // GET /api/v1/notifications read from the inbox.
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(RecordingAdapter::new(
                "telegram-1",
                Arc::clone(&seen),
            )))
            .await;
        let (routes, _panel) = attach_test_routes(&router).await;
        routes
            .set(
                agentos_types::NotificationEvent::SystemAlert,
                "telegram-1",
                crate::notification_routes::RouteMode::Never,
            )
            .await
            .expect("set rule");

        let msg = kernel_msg("kept anyway");
        let id = msg.id;
        router.deliver(msg).await.expect("deliver");

        assert!(seen.read().await.is_empty());
        let stored = router.inbox().get(&id).await.expect("inbox read");
        assert!(
            stored.is_some(),
            "suppressed message must still be in the inbox"
        );
    }

    #[tokio::test]
    async fn deliver_filtered_applies_the_same_gate() {
        // The escalation sink's fan-out uses deliver_filtered — gating only
        // `deliver` would leave every approval ungated.
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(RecordingAdapter::new(
                "telegram-1",
                Arc::clone(&seen),
            )))
            .await;
        let (routes, _panel) = attach_test_routes(&router).await;
        routes
            .set(
                agentos_types::NotificationEvent::Approval,
                "telegram-1",
                crate::notification_routes::RouteMode::Never,
            )
            .await
            .expect("set rule");

        let mut msg = kernel_msg("approve me");
        msg.thread_id = Some("escalation:7".to_string());
        let ids: std::collections::HashSet<String> =
            ["telegram-1".to_string()].into_iter().collect();
        router
            .deliver_filtered(msg, &ids, &Default::default())
            .await
            .expect("deliver_filtered");

        assert!(
            seen.read().await.is_empty(),
            "an explicitly selected channel must still obey a Never rule"
        );
    }

    #[tokio::test]
    async fn when_away_suppresses_while_a_panel_is_connected() {
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(RecordingAdapter::new(
                "telegram-1",
                Arc::clone(&seen),
            )))
            .await;
        let (routes, panel) = attach_test_routes(&router).await;
        routes
            .set(
                agentos_types::NotificationEvent::SystemAlert,
                "telegram-1",
                crate::notification_routes::RouteMode::WhenAway,
            )
            .await
            .expect("set rule");

        panel.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        router
            .deliver(kernel_msg("while here"))
            .await
            .expect("deliver");
        assert!(seen.read().await.is_empty(), "panel open -> suppressed");

        panel.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        router
            .deliver(kernel_msg("while away"))
            .await
            .expect("deliver");
        assert_eq!(seen.read().await.len(), 1, "panel closed -> delivered");
    }

    #[tokio::test]
    async fn send_to_channel_routes_delivery_stack_kind_to_its_adapter() {
        // Telegram/Ntfy/Email are owned by this router's DeliveryAdapters.
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(RecordingAdapter::new(
                "telegram-1",
                Arc::clone(&seen),
            )))
            .await;
        router.attach_channel_manager(empty_channel_manager());

        router
            .send_to_channel(kernel_msg("hello telegram"), "telegram-1")
            .await
            .expect("delivery-stack send must succeed");
        let delivered = seen.read().await.clone();
        assert_eq!(delivered, vec!["hello telegram".to_string()]);
    }

    #[tokio::test]
    async fn send_to_channel_routes_manager_stack_kind_past_the_delivery_adapters() {
        // Discord/Slack/WhatsApp/Webhook have no DeliveryAdapter, so the send
        // must fall through to the ChannelManager. Nothing is registered there
        // either, so the manager's own "channel not found" surfaces — which is
        // the proof that the manager stack was the one consulted. Before this
        // fix the send stopped at the delivery stack and returned Ok(()).
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(RecordingAdapter::new(
                "telegram-1",
                Arc::clone(&seen),
            )))
            .await;
        router.attach_channel_manager(empty_channel_manager());

        let err = router
            .send_to_channel(kernel_msg("hello discord"), "discord-1")
            .await
            .expect_err("unregistered manager channel must report failure");
        assert!(
            err.to_string().contains("not found"),
            "expected the ChannelManager's error, got: {err}"
        );
        assert!(
            seen.read().await.is_empty(),
            "must not have been delivered to the Telegram adapter"
        );
    }

    /// `deliver_to_channel` deliberately returns `Ok(())` when no adapter owns
    /// the instance. Boot restore leaves a channel `active: true` in the
    /// registry when `build_channel_adapter` fails (an ntfy topic on
    /// `http://ntfy.local` is now rejected by the SSRF blocklist), so the
    /// agent-facing `channel-send` used to pass the `active` check, find no
    /// adapter, and tell the agent the operator had been notified.
    /// `send_to_channel` must fail closed instead.
    #[tokio::test]
    async fn send_to_channel_fails_closed_when_the_adapter_never_registered() {
        let router = test_router();
        router.attach_channel_manager(empty_channel_manager());

        assert!(
            router
                .deliver_to_channel(kernel_msg("silent"), "ntfy-1")
                .await
                .is_ok(),
            "deliver_to_channel swallows this by design — that is why the \
             agent-facing send must not use it"
        );

        let err = router
            .send_to_channel(kernel_msg("silent"), "ntfy-1")
            .await
            .expect_err("an unregistered instance must not report success");
        assert!(
            err.to_string().contains("not found"),
            "expected a delivery failure, got: {err}"
        );
    }

    #[tokio::test]
    async fn send_to_channel_errors_when_no_stack_owns_the_channel() {
        let router = test_router();
        assert!(router
            .send_to_channel(kernel_msg("nowhere"), "unknown-1")
            .await
            .is_err());
    }

    #[test]
    fn outbound_from_uses_platform_thread_and_avoids_duplicate_subject() {
        // Subject is a truncated copy of the body (how InboundRouter fills it):
        // do not repeat it as a header.
        let mut msg = kernel_msg("just the body");
        msg.reply_to_external_id = Some("42".to_string());
        let out = outbound_from(&msg, "discord-1");
        assert_eq!(out.thread_id.as_deref(), Some("42"));
        // `Text`, not `Markdown`: the Webhook adapter serializes this variant
        // verbatim into an HMAC-signed body, so the tag is a published contract.
        assert!(matches!(&out.content, MessageContent::Text(t) if t == "just the body"));

        // A distinct subject (scheduled delivery) is rendered as a header.
        msg.subject = "Nightly backup".to_string();
        let out = outbound_from(&msg, "discord-1");
        assert!(
            matches!(&out.content, MessageContent::Text(t) if t == "**Nightly backup**\n\njust the body")
        );
        assert_eq!(
            serde_json::to_value(&out.content).expect("serialize")["type"],
            "Text",
            "webhook consumers key off this discriminant"
        );
    }

    /// An adapter that reports itself unavailable must fail the send, not
    /// return `Ok(())` — `channel-send` answers `{"status":"delivered"}` and
    /// writes a `ChannelMessageSent` audit row on `Ok`. Hits the Email stub
    /// permanently and Telegram until its `chat_id` is discovered.
    #[tokio::test]
    async fn send_to_channel_errors_when_the_adapter_is_unavailable() {
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        let mut adapter = RecordingAdapter::new("email-1", Arc::clone(&seen));
        adapter.available = false;
        router.register_adapter(Arc::new(adapter)).await;
        router.attach_channel_manager(empty_channel_manager());

        let err = router
            .send_to_channel(kernel_msg("into the void"), "email-1")
            .await
            .expect_err("an unavailable adapter must not report success");
        assert!(
            err.to_string().contains("unavailable"),
            "expected an unavailability error, got: {err}"
        );
        assert!(seen.read().await.is_empty(), "nothing may have been sent");

        // `deliver_to_channel` shares the same helper, so it fails closed too.
        assert!(router
            .deliver_to_channel(kernel_msg("into the void"), "email-1")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn typing_on_channel_pings_only_the_named_adapter_and_never_errors() {
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        let target = Arc::new(RecordingAdapter::new("telegram-1", seen.clone()));
        let bystander = Arc::new(RecordingAdapter::new("telegram-2", seen.clone()));
        router.register_adapter(target.clone()).await;
        router.register_adapter(bystander.clone()).await;

        assert!(router.typing_on_channel("telegram-1").await);
        assert!(router.typing_on_channel("telegram-1").await);

        assert_eq!(target.typing_count(), 2);
        // A ping must never fan out — the indicator belongs to one chat.
        assert_eq!(bystander.typing_count(), 0);

        // An unknown instance is a silent no-op, not a panic: the keepalive
        // fires on a timer and can outlive a channel being disconnected. It
        // reports `false` so the caller stops rather than spinning all turn.
        assert!(!router.typing_on_channel("does-not-exist").await);

        // Cosmetic pings must not be mistaken for delivered messages.
        assert!(seen.read().await.is_empty());
    }

    #[tokio::test]
    async fn typing_is_suppressed_while_a_blocking_question_is_outstanding() {
        // A turn waiting on the operator is not working. Showing "typing…" there
        // tells the user to keep waiting for a reply that cannot arrive until
        // they answer the question sitting in the same chat.
        let router = test_router();
        let seen = Arc::new(RwLock::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter::new("telegram-1", seen));
        router.register_adapter(adapter.clone()).await;

        assert!(router.typing_on_channel("telegram-1").await);
        assert_eq!(adapter.typing_count(), 1);

        let mut blocking = kernel_msg("approve this?");
        blocking.interaction = Some(agentos_types::InteractionRequest {
            blocking: true,
            timeout_secs: 60,
            auto_action: String::new(),
            max_concurrent: 3,
        });
        let _rx = router.deliver(blocking).await.expect("deliver");

        // Suppressed, but reported as `true`: the turn resumes once the user
        // answers, and the indicator should come back with it.
        assert!(router.typing_on_channel("telegram-1").await);
        assert_eq!(
            adapter.typing_count(),
            1,
            "indicator claimed the agent was working while it was blocked on a human"
        );
    }
}
