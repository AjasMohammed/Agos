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
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_channels::manager::ChannelManager;
use agentos_channels::pairing::PairingManager;
use agentos_channels::types::{MessageContent, OutboundMessage};
use agentos_types::TraceID;
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

    /// Render a human-readable approval prompt for the given escalation.
    /// Includes the escalation id, urgency, decision_point, and
    /// instructions for the `/approve` and `/deny` reply commands.
    fn render(esc: &PendingEscalation) -> String {
        let preview = esc.context_summary.chars().take(280).collect::<String>();
        let preview = if esc.context_summary.chars().count() > 280 {
            format!("{preview}…")
        } else {
            preview
        };
        let decision: String = esc.decision_point.chars().take(240).collect();
        let decision = if esc.decision_point.chars().count() > 240 {
            format!("{decision}…")
        } else {
            decision
        };
        let expires_in_secs = (esc.expires_at - chrono::Utc::now()).num_seconds().max(0);
        format!(
            "🛂 AgentOS approval needed (#{id})\n\
             Urgency: {urgency}\n\
             Decision: {decision}\n\
             Context: {preview}\n\n\
             Reply `/approve {id}` or `/deny {id}` (expires in ~{exp}s)",
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

        let approved = self.pairing.list_approved().await;
        if approved.is_empty() {
            tracing::debug!(
                escalation_id = escalation.id,
                "ChannelBroadcastSink: no paired senders — skipping"
            );
            return;
        }

        let body = Self::render(escalation);
        for sender in approved {
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

            let msg = OutboundMessage {
                channel_instance_id: sender.channel_id.clone(),
                content: MessageContent::Markdown(body.clone()),
                thread_id: None,
            };
            if let Err(e) = self.channels.send(&sender.channel_id, msg).await {
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

    #[test]
    fn render_includes_id_and_commands() {
        let esc = fixture(42);
        let body = ChannelBroadcastSink::render(&esc);
        assert!(body.contains("#42"), "body must contain escalation id");
        assert!(
            body.contains("/approve 42"),
            "body must include approve cmd"
        );
        assert!(body.contains("/deny 42"), "body must include deny cmd");
        assert!(body.contains("install of python3"));
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

    #[test]
    fn render_truncates_long_context() {
        let mut esc = fixture(7);
        esc.context_summary = "x".repeat(1000);
        let body = ChannelBroadcastSink::render(&esc);
        assert!(
            body.contains("…"),
            "long context should be ellipsis-truncated"
        );
        // Truncated preview should not exceed ~330 chars including formatting.
        let preview_line = body
            .lines()
            .find(|l| l.starts_with("Context:"))
            .expect("Context line present");
        assert!(preview_line.chars().count() < 350);
    }
}
