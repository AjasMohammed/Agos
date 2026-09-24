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
use crate::escalation_card::EscalationCard;
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

/// Default dedupe window: escalations with an identical [`dedupe_key`]
/// broadcast within this window are suppressed. Defends against
/// retry storms where an agent repeatedly creates the same logical
/// escalation under fresh ids (review finding I6 — id-based dedupe was
/// dead code because `EscalationManager` always allocates a new id).
///
/// Applies to non-blocking escalations only — see `broadcast`.
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

/// Stable dedupe key for an escalation. Two non-blocking escalations whose
/// key matches within the dedupe window are treated as a retry and the second
/// one is suppressed at the channel sink — operators only see the first
/// prompt. Blocking escalations record their key but are always broadcast;
/// see `broadcast`.
///
/// `metadata` is in the key because `decision_point` does not identify the ask.
/// It is a human sentence — "Allow OSS to use 'audio'?" — and where it does
/// name a target, `describe_target` picks exactly one payload key, so the rest
/// of the payload is invisible to it. On 2026-09-19 that made two `audio` calls
/// (`volume` and `playback`) share a key, and two `channel-send` calls share
/// one too: both named the same attachment path as their target, so the
/// channel they were sending it *to* never entered the key. `metadata` carries
/// what the sentence drops — the redacted, compacted payload plus tool name for
/// a tool approval (`approval_hook`), the device and operation for a HAL prompt.
///
/// What this buys today is the *seed*, not the suppression: blocking
/// escalations are exempt from suppression (see `broadcast`) but still record
/// their key, and without the payload a blocking `volume` approval seeded a key
/// broad enough to swallow a later, genuinely different non-blocking ask from
/// the same task. It also means the incident's collapses stay impossible if the
/// blocking exemption is ever narrowed.
///
/// Hashed rather than kept readable: the key embeds a tool payload, and
/// `broadcast` logs it. `redact_secret_fields` is a keyword allowlist, not a
/// guarantee — it deliberately skips `key`, and it does not touch payload
/// *content* such as a file body or a shell command. Nothing parses the key, so
/// there is nothing to lose by making it opaque. Collisions are irrelevant on a
/// map this size (`MAX_ESCALATIONS_PER_TASK` entries, 30s TTL).
///
/// Object key ordering is normalised, not merely deterministic: `serde_json` is
/// built without `preserve_order`, so `Map` is a `BTreeMap` and `Display` sorts.
/// An agent re-emitting the same payload with its fields in another order still
/// lands on the same key. `dedupe_key_separates_same_sentence_different_payload`
/// pins that.
fn dedupe_key(esc: &PendingEscalation) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        esc.task_id.to_string(),
        esc.agent_id.to_string(),
        &esc.decision_point,
        esc.metadata.to_string(),
    )
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Broadcasts every new `PendingEscalation` to all paired DM channels.
///
/// Defends against:
///   - Chat spam: per-(channel, sender) rate limit (default 6/min). This is
///     the one defence that can still withhold a *blocking* card — it drops
///     the whole fan-out for that sender, `UserInbox` write included, so the
///     escalation page is the only surface left. It logs and files an
///     `EscalationBroadcastSuppressed` audit entry when it does.
///   - Retry storms: dedupe identical (task, agent, decision, payload) asks
///     (non-blocking only — a live gate is always broadcast)
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

    /// The redacted card: what third-party delivery targets (ntfy, webhook,
    /// desktop) and the inbox receive.
    ///
    /// `context_summary` embeds the redacted tool payload and the task text,
    /// and these targets are operator-configured third-party endpoints, so the
    /// summary names *what* is being approved — question, agent, tool, risk —
    /// and withholds the rest. See [`EscalationCard::to_summary_markdown`].
    fn render_summary(esc: &PendingEscalation) -> String {
        EscalationCard::from_escalation(esc).to_summary_markdown()
    }

    /// The full card, for chats paired to the operator: a Telegram private
    /// chat or a paired DM. See [`EscalationCard::to_markdown`].
    ///
    /// The `/approve` and `/deny` instructions are deliberately absent: they
    /// come from `UserMessage.actions` at the adapter, either as native
    /// controls or via `render_actions_fallback`.
    fn render(esc: &PendingEscalation) -> String {
        EscalationCard::from_escalation(esc).to_markdown()
    }
}

#[async_trait::async_trait]
impl BroadcastSink for ChannelBroadcastSink {
    async fn broadcast(&self, escalation: &PendingEscalation) {
        self.maybe_housekeep().await;

        // Dedupe: an identical ask (see `dedupe_key`) within the window →
        // suppress. Only fires for genuine retries, not for distinct
        // escalations that happen to share an id or a tool name.
        //
        // Blocking escalations are exempt from the *suppression*. The window
        // damps *notification* retry storms, but a blocking escalation is a
        // live gate with its own id: dropping its card removes the only
        // surface the operator can act on while the agent's tool call stays
        // parked, so the decision has to be chased down in the web panel
        // instead. Seen live on 2026-09-19 — an agent retrying `audio` inside
        // 30s had five cards dropped and every one had to be resolved from the
        // panel. The per-task escalation cap (`MAX_ESCALATIONS_PER_TASK`) and
        // the rate limits below still bound the volume this window was added
        // for.
        //
        // They are NOT exempt from *recording* the key: `already_broadcast`
        // checks and inserts in one step, so short-circuiting it would let a
        // later non-blocking repeat of the same decision through, and let an
        // agent defeat the window by alternating `blocking`.
        let key = dedupe_key(escalation);
        let duplicate = self.already_broadcast(&key).await;
        if !escalation.blocking && duplicate {
            tracing::debug!(
                escalation_id = escalation.id,
                dedupe_key = %key,
                "ChannelBroadcastSink: duplicate escalation — suppressing"
            );
            self.audit_suppressed(escalation, "duplicate", None, None);
            return;
        }

        let body = Self::render(escalation);
        let summary = Self::render_summary(escalation);

        // Channels already registered as delivery adapters are reached by the
        // router fan-out below; their paired senders must be skipped or the
        // operator gets the same prompt twice on the same channel. Populated
        // only once the fan-out call itself succeeded — note that
        // `deliver_filtered` reports `Ok(())` even when an individual adapter's
        // send failed (it records `DeliveryStatus::Failed` per adapter), so
        // this covers "the router took it", not "every adapter delivered".
        // A channel the routing matrix muted is never added, and the
        // paired-DM loop below re-checks the matrix itself.
        let mut covered_by_router = std::collections::HashSet::new();

        // Who may see the full card. Router adapters answer for themselves
        // (`is_private_chat`: a Telegram 1:1 chat yes, a group or ntfy topic
        // no). ChannelManager channels are not router adapters; reaching them
        // at all takes an operator-approved DM pairing, which is the trust
        // this sink has always extended to them.
        // ponytail: a manager-stack pairing on a guild channel still gets the
        // full card — add `is_private_chat` to `ChannelAdapter` if that bites.
        let (private_chats, (other_ids, other_kinds)) = match self.notification_router.get() {
            Some(router) => router.split_private_chats().await,
            None => Default::default(),
        };

        // Fan out through the notification router FIRST. `deliver_filtered`
        // persists to `UserInbox` (so the prompt shows up in the panel's
        // notification bell, not only on the escalation page) and reaches every
        // non-private delivery adapter — desktop, ntfy, email, webhook — with
        // the summary. It runs before the private-chat sends so a slow chat
        // cannot push the inbox write past the sink's 30s budget.
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
                // Bound the fan-out. `EscalationManager` wraps this whole
                // broadcast in a 30s timeout and `WebhookDeliveryAdapter`
                // retries with backoff — one misconfigured webhook URL would
                // otherwise burn the entire budget and the sends below would
                // never run.
                let sent = tokio::time::timeout(
                    ROUTER_FANOUT_TIMEOUT,
                    router.deliver_filtered(
                        Self::as_user_message(escalation, summary.clone()),
                        &other_ids,
                        &other_kinds,
                    ),
                )
                .await;
                match sent {
                    Ok(Ok(())) => covered_by_router.extend(other_ids.iter().cloned()),
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

                // Operator private chats get the full card. One router
                // `deliver` used to send the summary everywhere, so a Telegram
                // DM never saw what it was approving.
                for id in &private_chats {
                    // The routing matrix gates this path too. `deliver_filtered`
                    // above covers only the non-private adapters, so without
                    // this check muting approvals on a Telegram DM in the panel
                    // would still DM you.
                    if let Some(routes) = router.routes() {
                        if !routes.allows(agentos_types::NotificationEvent::Approval, id) {
                            self.audit_suppressed(escalation, "route_muted", Some(id), None);
                            continue;
                        }
                    }
                    let sent = tokio::time::timeout(
                        ROUTER_FANOUT_TIMEOUT,
                        router.send_to_channel(Self::as_user_message(escalation, body.clone()), id),
                    )
                    .await;
                    match sent {
                        Ok(Ok(())) => {
                            covered_by_router.insert(id.clone());
                        }
                        Ok(Err(e)) => tracing::warn!(
                            escalation_id = escalation.id,
                            channel = %id,
                            error = %e,
                            "Escalation prompt to private chat failed — falling back to paired DMs"
                        ),
                        Err(_) => tracing::warn!(
                            escalation_id = escalation.id,
                            channel = %id,
                            "Escalation prompt to private chat timed out — falling back to paired DMs"
                        ),
                    }
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

            // Routing matrix: the operator muted approvals on this channel.
            if let Some(routes) = self.notification_router.get().and_then(|r| r.routes()) {
                if !routes.allows(
                    agentos_types::NotificationEvent::Approval,
                    &sender.channel_id,
                ) {
                    tracing::debug!(
                        escalation_id = escalation.id,
                        channel = %sender.channel_id,
                        "Approval muted for this channel by the notification routing matrix"
                    );
                    self.audit_suppressed(
                        escalation,
                        "route_muted",
                        Some(&sender.channel_id),
                        Some(&sender.sender_id),
                    );
                    continue;
                }
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
            // The fallback must not widen the audience: a router adapter that
            // is not a private chat (a Telegram group whose summary send failed)
            // gets the summary here too, never the full card.
            let text = if other_ids.contains(&sender.channel_id) {
                &summary
            } else {
                &body
            };
            let send = async {
                match self.notification_router.get() {
                    Some(router) => {
                        router
                            .send_to_channel(
                                Self::as_user_message(escalation, text.clone()),
                                &sender.channel_id,
                            )
                            .await
                    }
                    None => {
                        let msg = OutboundMessage {
                            actions: crate::escalation_prompt::escalation_actions(escalation),
                            channel_instance_id: sender.channel_id.clone(),
                            content: MessageContent::Markdown(text.clone()),
                            thread_id: None,
                        };
                        self.channels.send(&sender.channel_id, msg).await
                    }
                }
            };
            let send_result = match tokio::time::timeout(ROUTER_FANOUT_TIMEOUT, send).await {
                Ok(r) => r,
                Err(_) => {
                    tracing::warn!(
                        escalation_id = escalation.id,
                        channel = %sender.channel_id,
                        "ChannelBroadcastSink: paired send timed out"
                    );
                    continue;
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

    async fn resolved(&self, escalation_id: u64) {
        if let Some(router) = self.notification_router.get() {
            router
                .retract_actions(&format!("escalation:{escalation_id}"))
                .await;
        }
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
            // `json!({})`, not `Null` — that is what `default_metadata()`
            // gives every escalation created without explicit metadata.
            metadata: serde_json::json!({}),
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

    /// Two asks that render the same sentence but request different things
    /// must never share a key.
    ///
    /// Regression for 2026-09-19: `decision_point` for a tool approval is
    /// "Allow <agent> to use '<tool>'?" and names no payload, so keying on it
    /// alone collapsed `{"action":"volume"}` with `{"action":"playback"}`, and
    /// a `channel-send` to `telegram-main` with one to `Buddy`. Five cards
    /// were dropped in one conversation and not one of them was a duplicate.
    #[test]
    fn dedupe_key_separates_same_sentence_different_payload() {
        let mut volume = fixture(1);
        let mut playback = fixture(2);
        playback.task_id = volume.task_id;
        playback.agent_id = volume.agent_id;

        // Verbatim from the incident: identical sentence, different ask.
        let sentence = "Allow OSS to use 'audio'?";
        volume.decision_point = sentence.into();
        playback.decision_point = sentence.into();
        volume.metadata = serde_json::json!({
            "kind": "tool_approval",
            "tool_name": "audio",
            "input": { "action": "volume", "node_id": "49", "volume": 1 },
        });
        playback.metadata = serde_json::json!({
            "kind": "tool_approval",
            "tool_name": "audio",
            "input": { "action": "playback", "audio_path": "/inbox/voice.ogg" },
        });

        assert_ne!(
            dedupe_key(&volume),
            dedupe_key(&playback),
            "approving the volume call must not hide the playback gate"
        );

        // A true retry — same sentence, same payload — still collapses.
        let mut retry = fixture(3);
        retry.task_id = volume.task_id;
        retry.agent_id = volume.agent_id;
        retry.decision_point = sentence.into();
        retry.metadata = volume.metadata.clone();
        assert_eq!(dedupe_key(&volume), dedupe_key(&retry));

        // Field order must not matter. `serde_json` without `preserve_order`
        // backs `Map` with a `BTreeMap`, so `Display` sorts keys and an LLM
        // re-emitting the same payload in another order still hits the window.
        // Enabling that feature anywhere in the tree (features unify globally)
        // would silently break this; this assertion is the canary.
        retry.metadata = serde_json::json!({
            "input": { "volume": 1, "node_id": "49", "action": "volume" },
            "tool_name": "audio",
            "kind": "tool_approval",
        });
        assert_eq!(
            dedupe_key(&volume),
            dedupe_key(&retry),
            "reordering the payload's fields must not mint a new key"
        );
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

    /// Records what a delivery adapter was asked to send, with a switchable
    /// "is this a private chat" answer so both sink fan-out paths can be driven.
    struct SinkTestAdapter {
        instance_id: String,
        private: bool,
        seen: Arc<tokio::sync::RwLock<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::notification_router::DeliveryAdapter for SinkTestAdapter {
        fn channel_id(&self) -> agentos_types::DeliveryChannel {
            agentos_types::DeliveryChannel::custom("telegram".to_string())
        }
        async fn deliver(
            &self,
            msg: &UserMessage,
        ) -> Result<(), crate::notification_router::DeliveryError> {
            self.seen.write().await.push(msg.body.clone());
            Ok(())
        }
        async fn is_available(&self) -> bool {
            true
        }
        fn adapter_instance_id(&self) -> Option<String> {
            Some(self.instance_id.clone())
        }
        async fn is_private_chat(&self) -> bool {
            self.private
        }
    }

    /// A channel the operator muted for approvals must be reached by neither
    /// fan-out path: not the router summary (`deliver_filtered`) and not the
    /// paired-DM fallback. The two gates key off different strings — the
    /// adapter's `adapter_instance_id()` and `AllowedSender::channel_id` — and
    /// nothing else asserts they are the same channel key.
    async fn muted_channel_is_unreachable(private: bool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = Arc::new(
            crate::user_inbox::UserInbox::new(&dir.path().join("inbox.db"), 100)
                .expect("open user inbox"),
        );
        let audit = Arc::new(
            agentos_audit::AuditLog::open(&dir.path().join("audit.db")).expect("open audit log"),
        );
        let router = Arc::new(NotificationRouter::new(Arc::clone(&inbox), audit));

        let seen = Arc::new(tokio::sync::RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(SinkTestAdapter {
                instance_id: "telegram-muted".to_string(),
                private,
                seen: Arc::clone(&seen),
            }))
            .await;

        let store = Arc::new(
            crate::state_store::KernelStateStore::open(dir.path().join("state.db"))
                .await
                .expect("open state store"),
        );
        let routes = Arc::new(
            crate::notification_routes::RouteMatrix::load(
                store,
                Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            )
            .await
            .expect("load matrix"),
        );
        routes
            .set(
                agentos_types::NotificationEvent::Approval,
                "telegram-muted",
                crate::notification_routes::RouteMode::Never,
            )
            .await
            .expect("mute approvals");
        router.attach_routes(routes);

        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let channels = Arc::new(ChannelManager::new(
            tx,
            tokio_util::sync::CancellationToken::new(),
        ));
        let pairing = PairingManager::new();
        pairing
            .restore(vec![agentos_channels::pairing::AllowedSender {
                channel_id: "telegram-muted".to_string(),
                sender_id: "operator".to_string(),
                approved_at: chrono::Utc::now(),
                label: None,
            }])
            .await;
        let sink = ChannelBroadcastSink::new(channels, pairing);
        sink.attach_notification_router(&router);

        sink.broadcast(&fixture(42)).await;

        assert!(
            seen.read().await.is_empty(),
            "muted channel must get neither the router summary nor the paired DM \
             (private_chat = {private})"
        );
        // Muting delivery must never cost the record.
        let stored = inbox.list(false, 10).await.expect("inbox list");
        assert_eq!(stored.len(), 1, "the prompt still belongs in the inbox");
    }

    #[tokio::test]
    async fn muted_channel_gets_neither_the_summary_nor_the_dm() {
        muted_channel_is_unreachable(false).await;
    }

    #[tokio::test]
    async fn muted_private_chat_gets_neither_path_either() {
        muted_channel_is_unreachable(true).await;
    }

    /// The dedupe guarantee must survive the gate: an *unmuted* channel still
    /// gets exactly one prompt, not one per fan-out path.
    #[tokio::test]
    async fn unmuted_channel_still_gets_exactly_one_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = Arc::new(
            crate::user_inbox::UserInbox::new(&dir.path().join("inbox.db"), 100)
                .expect("open user inbox"),
        );
        let audit = Arc::new(
            agentos_audit::AuditLog::open(&dir.path().join("audit.db")).expect("open audit log"),
        );
        let router = Arc::new(NotificationRouter::new(Arc::clone(&inbox), audit));

        let seen = Arc::new(tokio::sync::RwLock::new(Vec::new()));
        router
            .register_adapter(Arc::new(SinkTestAdapter {
                instance_id: "telegram-open".to_string(),
                private: false,
                seen: Arc::clone(&seen),
            }))
            .await;

        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let channels = Arc::new(ChannelManager::new(
            tx,
            tokio_util::sync::CancellationToken::new(),
        ));
        let pairing = PairingManager::new();
        pairing
            .restore(vec![agentos_channels::pairing::AllowedSender {
                channel_id: "telegram-open".to_string(),
                sender_id: "operator".to_string(),
                approved_at: chrono::Utc::now(),
                label: None,
            }])
            .await;
        let sink = ChannelBroadcastSink::new(channels, pairing);
        sink.attach_notification_router(&router);

        sink.broadcast(&fixture(42)).await;

        assert_eq!(
            seen.read().await.len(),
            1,
            "the router fan-out covers this channel; the paired-DM loop must not resend"
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
            .find(|l| l.starts_with("xxx"))
            .expect("context line present");
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

    /// Build a sink wired to a real inbox, plus the inbox itself.
    async fn sink_with_inbox(
        dir: &std::path::Path,
    ) -> (ChannelBroadcastSink, Arc<crate::user_inbox::UserInbox>) {
        let inbox = Arc::new(
            crate::user_inbox::UserInbox::new(&dir.join("inbox.db"), 100).expect("open user inbox"),
        );
        let audit =
            Arc::new(agentos_audit::AuditLog::open(&dir.join("audit.db")).expect("open audit log"));
        let router = Arc::new(NotificationRouter::new(Arc::clone(&inbox), audit));
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let channels = Arc::new(ChannelManager::new(
            tx,
            tokio_util::sync::CancellationToken::new(),
        ));
        let sink = ChannelBroadcastSink::new(channels, PairingManager::new());
        sink.attach_notification_router(&router);
        // `_rx` drops here, closing the ChannelManager channel. Harmless while
        // the pairing manager has no approved senders, so nothing is ever sent
        // to the manager — a paired-sender test would have to return it.
        (sink, inbox)
    }

    /// Two blocking escalations sharing a dedupe key must BOTH be broadcast.
    ///
    /// Regression for 2026-09-19: an agent retrying `audio` inside the 30s
    /// window minted a fresh blocking escalation per retry with an identical
    /// `(task, agent, decision_point)` triple. The dedupe dropped every card
    /// after the first, so the operator approved the older prompt on Telegram
    /// while the live gate — a different id — stayed pending in the panel.
    #[tokio::test]
    async fn blocking_escalations_are_never_dedupe_suppressed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, inbox) = sink_with_inbox(dir.path()).await;

        let first = fixture(1);
        let mut second = fixture(2);
        second.task_id = first.task_id;
        second.agent_id = first.agent_id;
        second.decision_point = first.decision_point.clone();
        assert_eq!(dedupe_key(&first), dedupe_key(&second));
        assert!(first.blocking && second.blocking);

        sink.broadcast(&first).await;
        sink.broadcast(&second).await;

        let stored = inbox.list(false, 10).await.expect("inbox list");
        assert_eq!(
            stored.len(),
            2,
            "both blocking gates must reach the operator; got {stored:?}"
        );
        let threads: Vec<_> = stored
            .iter()
            .filter_map(|n| n.thread_id.as_deref())
            .collect();
        assert!(threads.contains(&"escalation:1"), "{threads:?}");
        assert!(threads.contains(&"escalation:2"), "{threads:?}");
    }

    /// The window still damps non-blocking retry storms — nothing is parked on
    /// those, so a repeat inside 30s is pure noise.
    #[tokio::test]
    async fn non_blocking_repeats_are_still_suppressed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, inbox) = sink_with_inbox(dir.path()).await;

        let mut first = fixture(1);
        let mut second = fixture(2);
        first.blocking = false;
        second.blocking = false;
        second.task_id = first.task_id;
        second.agent_id = first.agent_id;
        second.decision_point = first.decision_point.clone();

        sink.broadcast(&first).await;
        sink.broadcast(&second).await;

        let stored = inbox.list(false, 10).await.expect("inbox list");
        assert_eq!(stored.len(), 1, "the repeat must be suppressed");
        assert_eq!(stored[0].thread_id.as_deref(), Some("escalation:1"));
    }

    /// A blocking escalation is broadcast AND seeds the dedupe map, so a
    /// non-blocking repeat behind it is still suppressed. Without the seeding
    /// an agent could defeat the window by alternating `blocking`.
    #[tokio::test]
    async fn a_blocking_broadcast_still_seeds_the_dedupe_map() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, inbox) = sink_with_inbox(dir.path()).await;

        let first = fixture(1);
        let mut second = fixture(2);
        second.task_id = first.task_id;
        second.agent_id = first.agent_id;
        second.decision_point = first.decision_point.clone();
        second.blocking = false;
        assert!(first.blocking);

        sink.broadcast(&first).await;
        sink.broadcast(&second).await;

        let stored = inbox.list(false, 10).await.expect("inbox list");
        assert_eq!(
            stored.len(),
            1,
            "the non-blocking repeat must be suppressed; got {stored:?}"
        );
        assert_eq!(stored[0].thread_id.as_deref(), Some("escalation:1"));
    }

    /// The seed a blocking gate leaves behind must be payload-scoped, so it
    /// cannot swallow a later non-blocking ask that happens to render the same
    /// sentence. This is the regression the `metadata` dimension of
    /// [`dedupe_key`] exists to prevent — the seeding path is the one place it
    /// changes an outcome today.
    #[tokio::test]
    async fn a_blocking_seed_does_not_swallow_a_different_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, inbox) = sink_with_inbox(dir.path()).await;

        let mut first = fixture(1);
        let mut second = fixture(2);
        second.task_id = first.task_id;
        second.agent_id = first.agent_id;
        second.decision_point = first.decision_point.clone();
        second.blocking = false;
        first.metadata = serde_json::json!({
            "kind": "tool_approval", "tool_name": "audio",
            "input": { "action": "volume", "volume": 1 },
        });
        second.metadata = serde_json::json!({
            "kind": "tool_approval", "tool_name": "audio",
            "input": { "action": "playback", "audio_path": "/inbox/voice.ogg" },
        });
        assert!(first.blocking);

        sink.broadcast(&first).await;
        sink.broadcast(&second).await;

        let stored = inbox.list(false, 10).await.expect("inbox list");
        assert_eq!(
            stored.len(),
            2,
            "a different ask must not ride the blocking seed; got {stored:?}"
        );
    }
}
