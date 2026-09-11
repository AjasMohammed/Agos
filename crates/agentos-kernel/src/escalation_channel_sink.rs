//! Broadcast escalations to paired DM channels (Discord, Slack, Telegram,
//! Teams, Mattermost, Matrix, Line, WhatsApp, …).
//!
//! Without this sink, an agent that hits a `control_plane` tool such as
//! `host-package-install` stalls until the 5-minute escalation timeout
//! when the operator is not actively watching the web UI. By fanning every
//! pending escalation out to every paired channel sender, the operator
//! receives the prompt wherever they are.
//!
//! Pairing model: the sink broadcasts to every approved sender returned
//! by [`PairingManager::list_approved`]. There is currently no per-user
//! scoping — single-user deployments are the assumption. Multi-tenant
//! scoping is a follow-up (track `task_owner` against a paired user_id).

use crate::escalation::{BroadcastSink, PendingEscalation};
use crate::notification_router::NotificationRouter;
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_channels::manager::ChannelManager;
use agentos_channels::pairing::PairingManager;
use agentos_channels::types::{MessageContent, OutboundMessage};
use agentos_types::{
    NotificationID, NotificationPriority, NotificationSource, TraceID, UserMessage, UserMessageKind,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Default per-sender broadcast cap. A noisy agent can otherwise spam
/// every paired DM with successive escalation prompts. Tunable via
/// `[escalation.broadcast].per_sender_max_per_min` if/when the config
/// path is wired up.
const DEFAULT_PER_SENDER_MAX_PER_MIN: u32 = 6;
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Default dedupe window: identical (task_id, agent_id, decision_point)
/// triples broadcast within this window are suppressed. Defends against
/// retry storms where an agent repeatedly creates the same logical
/// escalation under fresh ids (review finding I6 — id-based dedupe was
/// dead code because `EscalationManager` always allocates a new id).
const DEFAULT_DEDUPE_WINDOW: Duration = Duration::from_secs(30);

/// Garbage-collect expired buckets only every Nth call so a flood of
/// broadcasts doesn't pay an O(map size) sweep on every check.
const GC_EVERY_N_CALLS: u32 = 32;

/// Idle-bucket eviction threshold for `rate_buckets`. Buckets whose
/// `window_start` is older than this are dropped at GC time so the map
/// is bounded by recent activity (review finding S1).
const RATE_BUCKET_IDLE_TTL: Duration = Duration::from_secs(300);

/// Ceiling on the notification-router fan-out. `EscalationManager` gives the
/// whole broadcast 30s; the paired-DM loop runs after the router path, so the
/// router must not be allowed to consume the entire budget (a webhook adapter
/// retries with backoff).
const ROUTER_FANOUT_TIMEOUT: Duration = Duration::from_secs(8);

/// Per-sender token bucket.
struct RateBucket {
    count: u32,
    window_start: Instant,
}

impl RateBucket {
    fn allow(&mut self, max_per_min: u32) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= RATE_WINDOW {
            self.count = 0;
            self.window_start = now;
        }
        if self.count >= max_per_min {
            return false;
        }
        self.count += 1;
        true
    }
}

/// Stable dedupe key for an escalation. Two escalations whose `(task_id,
/// agent_id, decision_point)` triple matches within the dedupe window
/// are treated as a retry and the second one is suppressed at the
/// channel sink — operators only see the first prompt.
fn dedupe_key(esc: &PendingEscalation) -> String {
    format!("{}|{}|{}", esc.task_id, esc.agent_id, esc.decision_point)
}

/// Truncate to `max` characters, marking the cut with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    let head: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{head}…")
    } else {
        head
    }
}

/// Broadcasts every new `PendingEscalation` to all paired DM channels.
///
/// Defends against:
///   - Chat spam: per-(channel, sender) rate limit (default 6/min).
///   - Retry storms: dedupe identical (task, agent, decision) triples
///     within a 30s window.
///   - Audit gap on suppression: every suppressed broadcast emits an
///     `EscalationBroadcastSuppressed` audit entry so the dashboard
///     surfaces what the operator missed (review finding I7).
pub struct ChannelBroadcastSink {
    channels: Arc<ChannelManager>,
    /// Kind-agnostic sender. `channels` alone only reaches Discord/Slack/
    /// WhatsApp/Webhook, so every Telegram/Ntfy operator was left staring at
    /// nothing while the agent parked for the full escalation timeout.
    /// Late-bound via [`BroadcastSink::attach_notification_router`].
    notification_router: std::sync::OnceLock<Arc<NotificationRouter>>,
    pairing: Arc<PairingManager>,
    audit: Option<Arc<AuditLog>>,
    per_sender_max_per_min: u32,
    dedupe_window: Duration,
    /// Per-(channel_id, sender_id) rate buckets.
    rate_buckets: Mutex<HashMap<(String, String), RateBucket>>,
    /// Last-seen instant per dedupe key.
    /// Bounded by `housekeep` running every Nth call.
    recent_broadcasts: Mutex<HashMap<String, Instant>>,
    /// Counter used to amortise the GC sweep.
    gc_counter: Mutex<u32>,
}

impl ChannelBroadcastSink {
    pub fn new(channels: Arc<ChannelManager>, pairing: Arc<PairingManager>) -> Self {
        Self::with_limits(
            channels,
            pairing,
            None,
            DEFAULT_PER_SENDER_MAX_PER_MIN,
            DEFAULT_DEDUPE_WINDOW,
        )
    }

    pub fn with_audit(
        channels: Arc<ChannelManager>,
        pairing: Arc<PairingManager>,
        audit: Arc<AuditLog>,
    ) -> Self {
        Self::with_limits(
            channels,
            pairing,
            Some(audit),
            DEFAULT_PER_SENDER_MAX_PER_MIN,
            DEFAULT_DEDUPE_WINDOW,
        )
    }

    pub fn with_limits(
        channels: Arc<ChannelManager>,
        pairing: Arc<PairingManager>,
        audit: Option<Arc<AuditLog>>,
        per_sender_max_per_min: u32,
        dedupe_window: Duration,
    ) -> Self {
        Self {
            channels,
            notification_router: std::sync::OnceLock::new(),
            pairing,
            audit,
            per_sender_max_per_min,
            dedupe_window,
            rate_buckets: Mutex::new(HashMap::new()),
            recent_broadcasts: Mutex::new(HashMap::new()),
            gc_counter: Mutex::new(0),
        }
    }

    /// Amortised maintenance: every Nth call evicts stale entries from
    /// both the dedupe map and the rate-bucket map. Without this the
    /// rate-bucket map is unbounded across kernel uptime.
    async fn maybe_housekeep(&self) {
        let mut counter = self.gc_counter.lock().await;
        *counter = counter.wrapping_add(1);
        if *counter % GC_EVERY_N_CALLS != 0 {
            return;
        }
        drop(counter);

        let now = Instant::now();
        {
            let mut dedupe = self.recent_broadcasts.lock().await;
            let window = self.dedupe_window;
            dedupe.retain(|_, t| now.duration_since(*t) < window);
        }
        {
            let mut buckets = self.rate_buckets.lock().await;
            buckets.retain(|_, b| now.duration_since(b.window_start) < RATE_BUCKET_IDLE_TTL);
        }
    }

    /// Record a logical-dedupe key and return `true` if the same key was
    /// observed within the dedupe window. Per-call GC keeps the map size
    /// bounded by the dedupe-window throughput.
    async fn already_broadcast(&self, key: &str) -> bool {
        let mut map = self.recent_broadcasts.lock().await;
        let now = Instant::now();
        let window = self.dedupe_window;
        // Inline GC for correctness even when housekeep hasn't run yet.
        map.retain(|_, t| now.duration_since(*t) < window);
        if map.contains_key(key) {
            return true;
        }
        map.insert(key.to_string(), now);
        false
    }

    /// Check the per-sender rate limit. Returns `true` if the broadcast is
    /// allowed; `false` (and the broadcast must be suppressed) otherwise.
    async fn allow_send(&self, channel_id: &str, sender_id: &str) -> bool {
        let mut buckets = self.rate_buckets.lock().await;
        let bucket = buckets
            .entry((channel_id.to_string(), sender_id.to_string()))
            .or_insert(RateBucket {
                count: 0,
                window_start: Instant::now(),
            });
        bucket.allow(self.per_sender_max_per_min)
    }

    /// Emit a typed audit entry whenever a broadcast is suppressed by the
    /// dedupe map or the rate limiter. Without this, an operator never
    /// learns that a `control_plane` approval prompt was withheld and
    /// the agent quietly ages into auto-deny.
    fn audit_suppressed(
        &self,
        escalation: &PendingEscalation,
        reason: &'static str,
        channel: Option<&str>,
        sender: Option<&str>,
    ) {
        let Some(audit) = &self.audit else {
            return;
        };
        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::EscalationBroadcastSuppressed,
            agent_id: Some(escalation.agent_id),
            task_id: Some(escalation.task_id),
            tool_id: None,
            details: serde_json::json!({
                "escalation_id": escalation.id,
                "reason": reason,
                "channel_instance_id": channel,
                "channel_sender": sender,
                "decision_point": escalation.decision_point,
            }),
            severity: AuditSeverity::Warn,
            reversible: false,
            rollback_ref: None,
        };
        if let Err(e) = audit.append(entry) {
            tracing::warn!(error = %e, "Failed to write EscalationBroadcastSuppressed audit");
        }
    }

    /// Wrap a rendered prompt as a `UserMessage` so it can go out through
    /// `NotificationRouter::send_to_channel`, which reaches both outbound
    /// stacks instead of just the `ChannelManager` half.
    ///
    /// `from: Kernel` is deliberate: `Agent` would put operator approval
    /// prompts under the router's 10-per-minute per-agent notification cap on
    /// top of this sink's own limiter, and a suppressed prompt parks the task
    /// for the full escalation timeout. `subject` is the prompt's own first
    /// line so no renderer prints a second header above it.
    fn as_user_message(esc: &PendingEscalation, body: String) -> UserMessage {
        UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Kernel,
            task_id: Some(esc.task_id),
            trace_id: esc.trace_id,
            kind: UserMessageKind::Notification,
            // A blocking approval prompt is always urgent — the task is parked.
            priority: NotificationPriority::Urgent,
            subject: body.lines().next().unwrap_or_default().to_string(),
            body,
            interaction: None,
            // The controls travel as data so each adapter can render them
            // natively; `render`/`render_summary` no longer spell them out.
            actions: crate::escalation_prompt::escalation_actions(esc),
            delivery_status: HashMap::new(),
            response: None,
            created_at: chrono::Utc::now(),
            expires_at: Some(esc.expires_at),
            read: false,
            thread_id: Some(format!("escalation:{}", esc.id)),
            reply_to_external_id: None,
            attachment: None,
        }
    }

    /// Render the redacted variant sent through `NotificationRouter`.
    ///
    /// The full prompt from [`Self::render`] embeds up to 280 characters of
    /// `context_summary`, which `ApprovalHook` builds as
    /// `"… Input preview: <raw tool input JSON>"` — for an `env-*` or
    /// secret-bearing call that is a plaintext credential. The router fans out
    /// to operator-configured third-party endpoints (webhook POST, Slack
    /// incoming webhook, desktop DBus), a target set that never saw escalation
    /// content before. So the router path names *what* is being approved —
    /// `decision_point`, which the kernel builds as "Tool 'x' (risk: y) awaiting
    /// approval" and which carries no tool input — but withholds
    /// `context_summary`; the payload preview stays on the explicitly paired DM
    /// channels. Without the decision point the prompt said only "urgent",
    /// which is unactionable: every adapter registered with the router (Telegram,
    /// ntfy, desktop, email, webhook) is marked `covered_by_router` and so never
    /// receives the full [`Self::render`] body.
    ///
    /// The body no longer spells out the `/approve` reply instructions: the
    /// controls ride along as `UserMessage.actions`, so an adapter with an
    /// interactive primitive draws buttons and one without appends the shared
    /// text fallback. That closes the gap where a router-only recipient (ntfy,
    /// webhook, desktop) was told a decision was needed but given no way to
    /// make it.
    fn render_summary(esc: &PendingEscalation) -> String {
        let expires_in_secs = (esc.expires_at - chrono::Utc::now()).num_seconds().max(0);
        format!(
            "🛂 AgentOS approval needed (#{id})\n\
             Urgency: {urgency}\n\
             Decision: {decision}\n\
             Open the escalation queue for the full context \
             (auto-denies in ~{exp}s).",
            id = esc.id,
            urgency = esc.urgency,
            decision = truncate(&esc.decision_point, 240),
            exp = expires_in_secs,
        )
    }

    /// Render a human-readable approval prompt for the given escalation.
    /// Includes the escalation id, urgency, decision_point and context.
    ///
    /// The `/approve` and `/deny` instructions are deliberately absent: they
    /// come from `UserMessage.actions` at the adapter, either as native
    /// controls or via `render_actions_fallback`. Spelling them out here too
    /// would print them twice on every text-only channel.
    fn render(esc: &PendingEscalation) -> String {
        // 700, not 280: `context_summary` is now a labelled Who/What/Where/Why
        // block rather than a JSON blob, and clipping at 280 cut it mid-"Why".
        let preview = truncate(&esc.context_summary, 700);
        let decision = truncate(&esc.decision_point, 240);
        let expires_in_secs = (esc.expires_at - chrono::Utc::now()).num_seconds().max(0);
        format!(
            "🛂 AgentOS approval needed (#{id})\n\
             Urgency: {urgency}\n\
             Decision: {decision}\n\
             Context: {preview}\n\n\
             Expires in ~{exp}s.",
            id = esc.id,
            urgency = esc.urgency,
            decision = decision,
            preview = preview,
            exp = expires_in_secs,
        )
    }
}

#[async_trait::async_trait]
impl BroadcastSink for ChannelBroadcastSink {
    async fn broadcast(&self, escalation: &PendingEscalation) {
        self.maybe_housekeep().await;

        // Dedupe: same logical (task, agent, decision) within the window
        // → suppress. Only fires for genuine retries, not for distinct
        // escalations that happen to share an id.
        let key = dedupe_key(escalation);
        if self.already_broadcast(&key).await {
            tracing::debug!(
                escalation_id = escalation.id,
                dedupe_key = %key,
                "ChannelBroadcastSink: duplicate escalation — suppressing"
            );
            self.audit_suppressed(escalation, "duplicate", None, None);
            return;
        }

        let body = Self::render(escalation);

        // Channels already registered as delivery adapters are reached by the
        // router fan-out below; their paired senders must be skipped or the
        // operator gets the same prompt twice on the same channel. Populated
        // only once `deliver` actually succeeded — on failure the paired-DM
        // loop is the fallback and must not be suppressed.
        let mut covered_by_router = std::collections::HashSet::new();

        // Fan out through the notification router FIRST. `deliver` persists to
        // `UserInbox` (so the prompt shows up in the panel's notification bell,
        // not only on the escalation page) and reaches every registered
        // delivery adapter — desktop, ntfy, email, webhook. Without this the
        // only delivery path was a paired DM, so an operator running panel-only
        // had to sit on the escalation page to notice that a task was parked.
        // `from: Kernel` keeps it out of the per-agent 10/min cap, so the
        // sink's own limiter is applied here instead.
        if let Some(router) = self.notification_router.get() {
            let agent_bucket = escalation.agent_id.to_string();
            if !self.allow_send("router", &agent_bucket).await {
                tracing::warn!(
                    escalation_id = escalation.id,
                    "ChannelBroadcastSink: router fan-out rate limit hit — suppressing"
                );
                self.audit_suppressed(escalation, "rate_limited", Some("router"), None);
            } else {
                let ids = router.adapter_instance_ids().await;
                // Bound the fan-out. `EscalationManager` wraps this whole
                // broadcast in a 30s timeout and `WebhookDeliveryAdapter`
                // retries with backoff — one misconfigured webhook URL would
                // otherwise burn the entire budget and the paired-DM loop
                // below would never run.
                let sent = tokio::time::timeout(
                    ROUTER_FANOUT_TIMEOUT,
                    router.deliver(Self::as_user_message(
                        escalation,
                        Self::render_summary(escalation),
                    )),
                )
                .await;
                match sent {
                    Ok(Ok(_)) => covered_by_router = ids,
                    Ok(Err(e)) => tracing::warn!(
                        escalation_id = escalation.id,
                        error = %e,
                        "Escalation fan-out failed — falling back to paired DMs"
                    ),
                    Err(_) => tracing::warn!(
                        escalation_id = escalation.id,
                        "Escalation fan-out timed out — falling back to paired DMs"
                    ),
                }
            }
        }

        let approved = self.pairing.list_approved().await;
        if approved.is_empty() {
            // Not fatal any more — the router fan-out above still reached the
            // inbox and every configured adapter. Still worth a warning when
            // there is no router either, because then nobody sees the prompt
            // and the task sits until it auto-denies.
            if self.notification_router.get().is_none() {
                tracing::warn!(
                    escalation_id = escalation.id,
                    "Escalation not delivered: no paired senders and no notification router — pair one with `agentos channel pair approve <code>`"
                );
            } else {
                tracing::debug!(
                    escalation_id = escalation.id,
                    "No paired DM senders; escalation delivered via notification adapters only"
                );
            }
            return;
        }
        for sender in approved {
            if covered_by_router.contains(&sender.channel_id) {
                tracing::debug!(
                    escalation_id = escalation.id,
                    channel = %sender.channel_id,
                    "Paired sender already covered by the router fan-out — skipping duplicate"
                );
                continue;
            }

            // Rate limit: skip senders that have already received the
            // configured number of broadcasts in this window.
            if !self.allow_send(&sender.channel_id, &sender.sender_id).await {
                tracing::warn!(
                    escalation_id = escalation.id,
                    channel = %sender.channel_id,
                    sender = %sender.sender_id,
                    "ChannelBroadcastSink: per-sender rate limit hit — suppressing"
                );
                self.audit_suppressed(
                    escalation,
                    "rate_limited",
                    Some(&sender.channel_id),
                    Some(&sender.sender_id),
                );
                continue;
            }

            // Prefer the kind-agnostic router: `self.channels` reaches only the
            // ChannelManager half (Discord/Slack/WhatsApp/Webhook) and errors
            // with "channel not found" for every Telegram/Ntfy sender. The
            // direct-manager path stays as a fallback for the (boot-order)
            // case where no channel has been connected yet — which also means
            // there are no paired senders, so it is effectively unreachable.
            let send_result = match self.notification_router.get() {
                Some(router) => {
                    router
                        .send_to_channel(
                            Self::as_user_message(escalation, body.clone()),
                            &sender.channel_id,
                        )
                        .await
                }
                None => {
                    let msg = OutboundMessage {
                        actions: crate::escalation_prompt::escalation_actions(escalation),
                        channel_instance_id: sender.channel_id.clone(),
                        content: MessageContent::Markdown(body.clone()),
                        thread_id: None,
                    };
                    self.channels.send(&sender.channel_id, msg).await
                }
            };
            if let Err(e) = send_result {
                tracing::warn!(
                    escalation_id = escalation.id,
                    channel = %sender.channel_id,
                    sender = %sender.sender_id,
                    error = %e,
                    "ChannelBroadcastSink: send failed"
                );
            }
        }
    }

    fn name(&self) -> &'static str {
        "channel"
    }

    fn attach_notification_router(&self, router: &Arc<NotificationRouter>) {
        let _ = self.notification_router.set(Arc::clone(router));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::PendingEscalation;
    use crate::kernel_action::EscalationReason;
    use agentos_types::*;

    fn fixture(id: u64) -> PendingEscalation {
        PendingEscalation {
            id,
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            reason: EscalationReason::AuthorizationRequired,
            context_summary: "Agent wants to install python3".into(),
            decision_point: "approve install of python3 via apt-get".into(),
            options: vec!["approve".into(), "deny".into()],
            urgency: "high".into(),
            blocking: true,
            trace_id: TraceID::new(),
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(300),
            auto_action: crate::escalation::AutoAction::Deny,
            metadata: serde_json::Value::Null,
            resolved: false,
            resolution: None,
            resolved_at: None,
        }
    }

    /// Every router-registered adapter (Telegram, ntfy, desktop, email,
    /// webhook) is marked `covered_by_router` and receives only this body, so
    /// it has to name the tool — "Urgency: high" alone tells the operator
    /// nothing about what they are approving.
    #[test]
    fn render_summary_names_the_decision_but_withholds_the_payload() {
        let esc = fixture(42);
        let body = ChannelBroadcastSink::render_summary(&esc);
        assert!(body.contains("#42"), "summary must carry the id: {body}");
        assert!(
            body.contains(&esc.decision_point),
            "summary must name the decision: {body}"
        );
        assert!(
            !body.contains("Agent wants to"),
            "summary must not leak the context preview: {body}"
        );
    }

    #[test]
    fn render_includes_id_and_context_but_not_the_commands() {
        let esc = fixture(42);
        let body = ChannelBroadcastSink::render(&esc);
        assert!(body.contains("#42"), "body must contain escalation id");
        assert!(body.contains("install of python3"));
        // The commands come from `actions` at the adapter. Emitting them here
        // too would print them twice on every text-only channel.
        assert!(
            !body.contains("/approve 42"),
            "body must not spell out the commands"
        );
    }

    #[test]
    fn both_render_paths_carry_the_controls() {
        // The full paired-DM prompt and the redacted router summary must both
        // be actionable — the router path reaches ntfy/webhook/desktop, which
        // previously got a prompt with no way to act on it.
        let esc = fixture(42);
        for body in [
            ChannelBroadcastSink::render(&esc),
            ChannelBroadcastSink::render_summary(&esc),
        ] {
            let msg = ChannelBroadcastSink::as_user_message(&esc, body);
            let cmds: Vec<&str> = msg.actions.iter().map(|a| a.command.as_str()).collect();
            assert_eq!(cmds, ["/approve 42", "/deny 42", "/approve 42 always"]);
        }
    }

    #[test]
    fn fallback_append_yields_each_command_exactly_once() {
        // Guards the double-instruction regression: `render` dropped its own
        // trailing line precisely so this append is the only source.
        let esc = fixture(42);
        let body = ChannelBroadcastSink::render(&esc);
        let msg = ChannelBroadcastSink::as_user_message(&esc, body);
        let full = format!(
            "{}{}",
            msg.body,
            agentos_types::render_actions_fallback(&msg.actions)
        );
        assert_eq!(full.matches("/approve 42 always").count(), 1);
        // "/approve 42" is a prefix of "/approve 42 always" — count the two
        // occurrences that implies, not three.
        assert_eq!(full.matches("/approve 42").count(), 2);
        assert_eq!(full.matches("/deny 42").count(), 1);
    }

    #[test]
    fn dedupe_key_collapses_distinct_ids_for_same_logical_escalation() {
        let mut a = fixture(1);
        let mut b = fixture(2);
        // Force the (task_id, agent_id) pair to match — otherwise they
        // come from `TaskID::new()` / `AgentID::new()` and would always
        // diverge.
        a.task_id = b.task_id;
        a.agent_id = b.agent_id;
        a.decision_point = "approve install of python3".into();
        b.decision_point = "approve install of python3".into();
        assert_eq!(dedupe_key(&a), dedupe_key(&b));

        // Different decision_point → distinct keys (operator must see both).
        b.decision_point = "approve install of nginx".into();
        assert_ne!(dedupe_key(&a), dedupe_key(&b));
    }

    #[tokio::test]
    async fn rate_limit_blocks_after_max() {
        // Build a sink without channels/pairing so we can exercise the
        // limiter directly. Use cheap stand-in `Arc<...>` values via
        // `tokio::sync::mpsc` since `ChannelManager::new` requires a
        // sender + cancellation token.
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let cancel = tokio_util::sync::CancellationToken::new();
        let channels = Arc::new(ChannelManager::new(tx, cancel));
        let pairing = PairingManager::new();
        let sink =
            ChannelBroadcastSink::with_limits(channels, pairing, None, 3, Duration::from_secs(30));

        // First three calls allowed, fourth blocked.
        for i in 0..3 {
            assert!(
                sink.allow_send("chan-A", "user-1").await,
                "call {i} should be allowed"
            );
        }
        assert!(
            !sink.allow_send("chan-A", "user-1").await,
            "fourth call exceeds the per-sender limit"
        );

        // Different sender on the same channel has its own bucket.
        assert!(sink.allow_send("chan-A", "user-2").await);
    }

    #[tokio::test]
    async fn dedupe_suppresses_repeats_within_window() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let cancel = tokio_util::sync::CancellationToken::new();
        let channels = Arc::new(ChannelManager::new(tx, cancel));
        let pairing = PairingManager::new();
        let sink =
            ChannelBroadcastSink::with_limits(channels, pairing, None, 10, Duration::from_secs(30));

        // First seen — not a duplicate.
        assert!(!sink.already_broadcast("k1").await);
        // Second within window — duplicate.
        assert!(sink.already_broadcast("k1").await);
        // Different key — not a duplicate.
        assert!(!sink.already_broadcast("k2").await);
    }

    #[tokio::test]
    async fn dedupe_garbage_collects_after_window() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let cancel = tokio_util::sync::CancellationToken::new();
        let channels = Arc::new(ChannelManager::new(tx, cancel));
        let pairing = PairingManager::new();
        // Tiny window so the test runs fast.
        let sink = ChannelBroadcastSink::with_limits(
            channels,
            pairing,
            None,
            10,
            Duration::from_millis(50),
        );

        assert!(!sink.already_broadcast("k").await);
        // Wait past the window and then re-broadcast — should NOT be dedupe-suppressed.
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            !sink.already_broadcast("k").await,
            "GC should drop the prior entry"
        );
    }

    #[tokio::test]
    async fn no_paired_senders_still_reaches_the_inbox() {
        // The panel reads `GET /api/v1/notifications` off the UserInbox. Before
        // the router fan-out, a panel-only operator with no paired DM channel
        // saw approval prompts on the escalation page and nowhere else.
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = Arc::new(
            crate::user_inbox::UserInbox::new(&dir.path().join("inbox.db"), 100)
                .expect("open user inbox"),
        );
        let audit = Arc::new(
            agentos_audit::AuditLog::open(&dir.path().join("audit.db")).expect("open audit log"),
        );
        let router = Arc::new(NotificationRouter::new(Arc::clone(&inbox), audit));

        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let channels = Arc::new(ChannelManager::new(
            tx,
            tokio_util::sync::CancellationToken::new(),
        ));
        let sink = ChannelBroadcastSink::new(channels, PairingManager::new());
        sink.attach_notification_router(&router);

        let esc = fixture(42);
        sink.broadcast(&esc).await;

        let stored = inbox.list(false, 10).await.expect("inbox list");
        assert_eq!(stored.len(), 1, "escalation must land in the user inbox");
        assert!(stored[0].body.contains("#42"));
        assert!(matches!(stored[0].priority, NotificationPriority::Urgent));
        // The router fans out to third-party webhooks and Slack; the raw tool
        // input preview that `render` embeds must NOT ride along.
        assert!(
            !stored[0].body.contains("install python3"),
            "router body must not carry the context/input preview"
        );
        // Correlation back to the escalation queue.
        assert_eq!(
            stored[0].thread_id.as_deref(),
            Some("escalation:42"),
            "inbox row must point back at the escalation"
        );
    }

    #[test]
    fn render_truncates_long_context() {
        let mut esc = fixture(7);
        esc.context_summary = "x".repeat(1000);
        let body = ChannelBroadcastSink::render(&esc);
        assert!(
            body.contains("…"),
            "long context should be ellipsis-truncated"
        );
        // Truncated preview should not exceed the 700-char clip plus formatting.
        let preview_line = body
            .lines()
            .find(|l| l.starts_with("Context:"))
            .expect("Context line present");
        assert!(preview_line.chars().count() < 750);
    }

    #[test]
    fn user_message_wrapper_preserves_prompt_and_avoids_double_header() {
        let esc = fixture(9);
        let body = ChannelBroadcastSink::render(&esc);
        let msg = ChannelBroadcastSink::as_user_message(&esc, body.clone());
        assert_eq!(msg.body, body, "prompt text must survive the wrapping");
        // Subject is the prompt's own first line, so no renderer stacks a
        // second header above it.
        assert!(msg.body.starts_with(&msg.subject));
        assert!(msg.subject.contains("#9"));
        // Kernel-sourced on purpose: an `Agent` source would put approval
        // prompts under the router's per-agent notification cap, and a
        // suppressed prompt parks the task for the full escalation timeout.
        assert!(matches!(msg.from, NotificationSource::Kernel));
    }
}
