use crate::kernel_action::EscalationReason;
use crate::state_store::KernelStateStore;
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};

/// Outcome delivered on the approval resolution channel for a pending
/// escalation. Used by the task executor to decide whether to retry the
/// tool call (Approved) or surface a denial to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionOutcome {
    Approved,
    Denied,
    /// Nobody answered before `expires_at`; the executor reports this as an
    /// expiry rather than an operator denial.
    Expired,
}

/// Normalize an operator-supplied escalation decision string into approve/deny.
///
/// Different surfaces produce different literals for the same intent — the CLI
/// `escalation resolve` and the interactive TTY prompt send `"approve"`, the
/// channel `/approve` path sends `"approved"`, and the REST API forwards
/// whatever the operator passed. Treat the common approval synonyms (any case)
/// as approval; everything else denies (fail-closed).
pub(crate) fn resolution_is_approval(resolution: &str) -> bool {
    // Leading word only: the REST path folds an operator note into the
    // string as `"approve (looks fine)"`, which must still count as approval
    // (previously it silently DENIED and failed the task).
    let head = resolution
        .trim()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    matches!(
        head.to_ascii_lowercase().as_str(),
        "approve" | "approved" | "allow" | "allowed"
    )
}

/// What should happen automatically when an escalation expires without human resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoAction {
    /// Default: deny the action (existing behavior).
    Deny,
    /// Soft-approval: approve the action automatically if no human intervenes.
    Approve,
}

/// Default escalation timeout in seconds (5 minutes per Spec §12).
pub(crate) const DEFAULT_ESCALATION_TIMEOUT_SECS: i64 = 300;

/// Maximum number of escalations a single task may create.
/// Looping agents can otherwise flood the escalation log with identical entries.
const MAX_ESCALATIONS_PER_TASK: usize = 5;

/// A pending escalation awaiting human review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEscalation {
    pub id: u64,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub reason: EscalationReason,
    pub context_summary: String,
    pub decision_point: String,
    pub options: Vec<String>,
    pub urgency: String,
    pub blocking: bool,
    pub trace_id: TraceID,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Escalation expires and auto-denies after this time (Spec §12).
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// What happens automatically on expiry: Deny (default) or Approve (soft-approval).
    #[serde(default = "default_auto_action")]
    pub auto_action: AutoAction,
    /// Optional structured metadata used by specialized workflows such as HAL approvals.
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    pub resolved: bool,
    pub resolution: Option<String>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn default_auto_action() -> AutoAction {
    AutoAction::Deny
}

fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Notification sink invoked when a new escalation is created. Implementations
/// fan an escalation out to a destination such as paired DM channels, an
/// HTTP webhook, or any other operator-reachable transport.
///
/// Sinks are best-effort — failures are logged but do not block escalation
/// creation. Each sink runs in its own `tokio::spawn` to keep the create
/// path non-blocking.
#[async_trait::async_trait]
pub trait BroadcastSink: Send + Sync {
    async fn broadcast(&self, escalation: &PendingEscalation);
    fn name(&self) -> &'static str;

    /// Late-bind the `NotificationRouter` so a sink can reach *both* outbound
    /// channel stacks (see `NotificationRouter::send_to_channel`). Sinks are
    /// constructed at kernel boot before every collaborator exists; this is
    /// called on the first channel connect/restore. Default: ignore.
    fn attach_notification_router(
        &self,
        _router: &Arc<crate::notification_router::NotificationRouter>,
    ) {
    }
}

/// Manages escalation requests from agents to human operators.
///
/// Stores pending escalations in memory (optionally backed by SQLite persistence).
/// Provides list/resolve operations for the CLI (`agentos escalation list/resolve`).
///
/// Escalations auto-deny after `DEFAULT_ESCALATION_TIMEOUT_SECS` (5 minutes)
/// if not resolved by a human operator (Spec §12: "Auto-action on expiry: deny").
pub struct EscalationManager {
    escalations: RwLock<Vec<PendingEscalation>>,
    next_id: RwLock<u64>,
    /// Configurable timeout in seconds. Defaults to 300 (5 minutes).
    timeout_secs: i64,
    /// Optional webhook URL: receives HTTP POST on escalation creation.
    notify_url: RwLock<Option<String>>,
    /// Optional persistence backend for escalation durability across restarts.
    state_store: Option<Arc<KernelStateStore>>,
    /// Out-of-band notification sinks (channels, push, etc.) invoked in
    /// parallel with the legacy `notify_url` webhook on every new
    /// escalation. Sinks are added at kernel boot via [`add_sink`].
    broadcast_sinks: RwLock<Vec<Arc<dyn BroadcastSink>>>,
    /// Pending oneshot senders for in-flight escalations awaiting human
    /// resolution. `ApprovalHook` calls [`prepare_resolution`] to install
    /// a pair, [`resolve`] consumes the sender to wake the waiting task,
    /// and the matching receiver is taken by `task_executor` (via
    /// [`take_resolution_receiver`]) so it can `.await` the outcome.
    /// Cleared on resolve / sweeper-expiry, so the map is bounded by
    /// the in-flight escalation count.
    pending_resolution_tx: RwLock<HashMap<u64, oneshot::Sender<ResolutionOutcome>>>,
    /// Audit sink for create/resolve events (set at kernel boot; None in unit tests).
    audit: RwLock<Option<Arc<AuditLog>>>,
    /// Receivers waiting for pickup by `task_executor`. Stored here so
    /// the receiver lifetime is decoupled from the hook fire path —
    /// hooks are sync-and-fire-and-forget; the awaiting happens later.
    pending_resolution_rx: RwLock<HashMap<u64, oneshot::Receiver<ResolutionOutcome>>>,
    /// Hardware registry, so that approving a `device_access` escalation
    /// actually grants the device. Without it the operator's decision was
    /// recorded and then dropped: `resolve` marked the escalation approved
    /// while the device stayed `Pending`, so the next HAL call raised a fresh
    /// escalation and the agent could never get through. Set at kernel boot;
    /// `None` in unit tests and in the CLI, where no HAL exists.
    hardware_registry: RwLock<Option<Arc<agentos_hal::HardwareRegistry>>>,
    /// Lossy realtime fan-out for the control panel's WebSocket layer. Set at
    /// kernel boot; `None` in the CLI and in unit tests. Without it the panel
    /// only learns about a new (or resolved) escalation on its next poll, so an
    /// operator who got the push notification instantly still stared at an
    /// empty approval queue for up to a full poll interval.
    realtime_tx: std::sync::OnceLock<tokio::sync::broadcast::Sender<RealtimeEvent>>,
}

impl EscalationManager {
    /// Attach the panel's realtime broadcast channel. Idempotent — a second
    /// call is ignored.
    pub fn set_realtime_sender(&self, tx: tokio::sync::broadcast::Sender<RealtimeEvent>) {
        let _ = self.realtime_tx.set(tx);
    }

    /// Push a coarse `escalations` event to the control panel so it refetches
    /// the queue immediately instead of waiting for its poll.
    ///
    /// At most the id travels: the escalation body can carry
    /// redacted-but-still-sensitive tool arguments, and every subscriber
    /// refetches through the `escalations:r`-scoped REST endpoint anyway.
    /// Bulk paths pass `None` and emit exactly one event rather than a
    /// fabricated id per row — a burst of N invalidations only makes the
    /// subscriber cancel and restart the same refetch N times.
    fn emit_realtime(&self, event: &str, id: Option<u64>) {
        if let Some(tx) = self.realtime_tx.get() {
            let data = match id {
                Some(id) => serde_json::json!({ "escalation_id": id }),
                None => serde_json::json!({}),
            };
            // `Err` = no subscribers connected; nothing to do.
            let _ = tx.send(RealtimeEvent {
                channel: "escalations".to_string(),
                event: event.to_string(),
                data,
            });
        }
    }

    /// Attach the audit log so approval decisions leave a durable trail.
    pub async fn set_audit_log(&self, audit: Arc<AuditLog>) {
        *self.audit.write().await = Some(audit);
    }

    async fn audit(&self, entry: AuditEntry) {
        if let Some(audit) = self.audit.read().await.as_ref() {
            if let Err(e) = audit.append(entry) {
                tracing::error!(error = %e, "Failed to write escalation audit entry");
            }
        }
    }

    fn audit_entry(
        event_type: AuditEventType,
        esc: &PendingEscalation,
        details: serde_json::Value,
    ) -> AuditEntry {
        AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: esc.trace_id,
            event_type,
            agent_id: Some(esc.agent_id),
            task_id: Some(esc.task_id),
            tool_id: None,
            details,
            severity: AuditSeverity::Security,
            reversible: false,
            rollback_ref: None,
        }
    }

    pub fn new() -> Self {
        Self::with_state_store(None)
    }

    pub fn with_state_store(state_store: Option<Arc<KernelStateStore>>) -> Self {
        Self {
            escalations: RwLock::new(Vec::new()),
            next_id: RwLock::new(1),
            timeout_secs: DEFAULT_ESCALATION_TIMEOUT_SECS,
            notify_url: RwLock::new(None),
            state_store,
            broadcast_sinks: RwLock::new(Vec::new()),
            pending_resolution_tx: RwLock::new(HashMap::new()),
            audit: RwLock::new(None),
            pending_resolution_rx: RwLock::new(HashMap::new()),
            hardware_registry: RwLock::new(None),
            realtime_tx: std::sync::OnceLock::new(),
        }
    }

    /// Attach the hardware registry so device-access approvals take effect.
    /// See [`Self::hardware_registry`].
    pub async fn set_hardware_registry(&self, registry: Arc<agentos_hal::HardwareRegistry>) {
        *self.hardware_registry.write().await = Some(registry);
    }

    /// Announce a new escalation on every registered `BroadcastSink`
    /// (channel DMs, push). Best-effort: each sink runs in its own task so a
    /// failure never blocks escalation creation, and each is bounded by
    /// `SINK_BROADCAST_TIMEOUT` so a misbehaving adapter (hung HTTP,
    /// unresponsive WebSocket) cannot leak detached tasks across kernel
    /// uptime (R3 finding I3).
    ///
    /// Called from BOTH creation paths. `create_device_access_escalation`
    /// bypasses `create_escalation_internal`, so before this was shared, a
    /// HAL device prompt reached the control panel and nothing else — an
    /// operator working over Telegram was never told the agent was waiting.
    async fn fan_out_to_sinks(&self, escalation: &PendingEscalation) {
        const SINK_BROADCAST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        let sinks = self.broadcast_sinks.read().await.clone();
        for sink in sinks {
            let esc = escalation.clone();
            tokio::spawn(async move {
                let sink_name = sink.name();
                if tokio::time::timeout(SINK_BROADCAST_TIMEOUT, sink.broadcast(&esc))
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        sink = sink_name,
                        escalation_id = esc.id,
                        timeout_secs = SINK_BROADCAST_TIMEOUT.as_secs(),
                        "BroadcastSink timed out; dropping the broadcast"
                    );
                }
            });
        }
    }

    /// Deliver `outcome` to whoever is parked on `escalation_id` and drop the
    /// channel. Every path that marks an escalation resolved must call this —
    /// a resolution that is recorded but never delivered leaves the waiter
    /// (`task_executor::enforce_tool_pre`, `KernelDeviceAccessGate::check`)
    /// parked until its own timeout on a decision that was already made.
    async fn wake_resolution(&self, escalation_id: u64, outcome: ResolutionOutcome) {
        if let Some(tx) = self
            .pending_resolution_tx
            .write()
            .await
            .remove(&escalation_id)
        {
            // `Err(_)` means the receiver was already dropped (waiter
            // cancelled or timed out) — nothing to do.
            let _ = tx.send(outcome);
        }
        // Clear any orphan receiver so the map doesn't leak.
        self.pending_resolution_rx
            .write()
            .await
            .remove(&escalation_id);
    }

    /// Install a oneshot resolution channel for `escalation_id`. Returns
    /// nothing — the sender stays inside the manager so [`resolve`] can
    /// consume it; the receiver is parked under the same key for later
    /// pickup via [`take_resolution_receiver`].
    ///
    /// Idempotent on duplicate calls: the second call replaces the prior
    /// pair (which is fine because the old receiver was unobserved).
    pub async fn prepare_resolution(&self, escalation_id: u64) {
        let (tx, rx) = oneshot::channel();
        self.pending_resolution_tx
            .write()
            .await
            .insert(escalation_id, tx);
        self.pending_resolution_rx
            .write()
            .await
            .insert(escalation_id, rx);
    }

    /// Take the receiver for `escalation_id` so the caller can `.await`
    /// the human resolution. Returns `None` if no resolution channel was
    /// installed for this id (e.g. escalation created without a waiter,
    /// or already taken by a competing executor).
    pub async fn take_resolution_receiver(
        &self,
        escalation_id: u64,
    ) -> Option<oneshot::Receiver<ResolutionOutcome>> {
        self.pending_resolution_rx
            .write()
            .await
            .remove(&escalation_id)
    }

    /// Register a broadcast sink. Sinks are invoked (concurrently) on every
    /// new escalation. Order of registration is preserved but not guaranteed
    /// to be the order of execution. Safe to call after kernel boot.
    pub async fn add_sink(&self, sink: Arc<dyn BroadcastSink>) {
        self.broadcast_sinks.write().await.push(sink);
    }

    /// Hand the `NotificationRouter` to every registered sink that wants one.
    /// Idempotent — called on each channel connect/restore.
    pub async fn attach_notification_router(
        &self,
        router: Arc<crate::notification_router::NotificationRouter>,
    ) {
        for sink in self.broadcast_sinks.read().await.iter() {
            sink.attach_notification_router(&router);
        }
    }

    async fn persist_escalation(&self, escalation: PendingEscalation) {
        let escalation_id = escalation.id;
        if let Some(store) = &self.state_store {
            if let Err(e) = store.upsert_escalation(escalation).await {
                tracing::error!(
                    escalation_id,
                    error = %e,
                    "Failed to persist escalation state"
                );
            }
        }
    }

    /// Restore unresolved escalations from SQLite at boot.
    pub async fn restore_from_store(&self) -> anyhow::Result<usize> {
        let Some(store) = &self.state_store else {
            return Ok(0);
        };

        let unresolved = store.load_unresolved_escalations().await?;
        let restored = unresolved.len();

        let mut next_id = store.next_escalation_id().await?;
        if let Some(max_loaded) = unresolved.iter().map(|e| e.id).max() {
            next_id = next_id.max(max_loaded.saturating_add(1));
        }
        if next_id == 0 {
            next_id = 1;
        }

        *self.escalations.write().await = unresolved;
        *self.next_id.write().await = next_id;

        Ok(restored)
    }

    /// Set a webhook URL that receives HTTP POST notifications on escalation creation.
    pub async fn set_notify_url(&self, url: Option<String>) {
        *self.notify_url.write().await = url;
    }

    /// Create a new escalation entry.
    ///
    /// If `auto_action` is `Some(AutoAction::Approve)`, the escalation becomes a
    /// "soft-approval" — it auto-approves on expiry instead of auto-denying.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_escalation(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        urgency: String,
        blocking: bool,
        trace_id: TraceID,
        auto_action: Option<AutoAction>,
    ) -> u64 {
        self.create_escalation_with_metadata(
            task_id,
            agent_id,
            reason,
            context_summary,
            decision_point,
            options,
            urgency,
            blocking,
            trace_id,
            auto_action,
            default_metadata(),
        )
        .await
    }

    /// [`Self::create_escalation`] with structured `metadata`. The approval
    /// hook uses this to record `{"kind":"tool_approval","tool_name",…}` so
    /// an "approve & remember" resolution can mint a standing grant without
    /// parsing the human-readable `decision_point` string.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_escalation_with_metadata(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        urgency: String,
        blocking: bool,
        trace_id: TraceID,
        auto_action: Option<AutoAction>,
        metadata: serde_json::Value,
    ) -> u64 {
        let (id, _rx) = self
            .create_escalation_internal(
                task_id,
                agent_id,
                reason,
                context_summary,
                decision_point,
                options,
                urgency,
                blocking,
                trace_id,
                auto_action,
                false,
                metadata,
            )
            .await;
        id
    }

    /// Create a new escalation and atomically install a resolution
    /// channel before any broadcast sinks fan out. Use this when the
    /// caller intends to park on the resolution receiver — the atomic
    /// install closes the race in which a fast user resolution
    /// (delivered via a sink that ran before the caller could call
    /// [`prepare_resolution`]) would silently drop the wake.
    ///
    /// Returns `(id, Some(rx))` on success. If the per-task escalation
    /// cap is hit, returns `(u64::MAX, None)` and the caller must abort.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_escalation_with_resolution(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        urgency: String,
        blocking: bool,
        trace_id: TraceID,
        auto_action: Option<AutoAction>,
    ) -> (u64, Option<oneshot::Receiver<ResolutionOutcome>>) {
        self.create_escalation_internal(
            task_id,
            agent_id,
            reason,
            context_summary,
            decision_point,
            options,
            urgency,
            blocking,
            trace_id,
            auto_action,
            true,
            default_metadata(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_escalation_internal(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        urgency: String,
        blocking: bool,
        trace_id: TraceID,
        auto_action: Option<AutoAction>,
        install_resolution: bool,
        metadata: serde_json::Value,
    ) -> (u64, Option<oneshot::Receiver<ResolutionOutcome>>) {
        // Acquire the write lock once so the cap check and push are atomic,
        // preventing a TOCTOU race where two concurrent callers both pass the check.
        let mut escalations = self.escalations.write().await;
        let task_count = escalations
            .iter()
            .filter(|e| e.task_id == task_id && !e.resolved)
            .count();
        if task_count >= MAX_ESCALATIONS_PER_TASK {
            tracing::warn!(
                task_id = %task_id,
                count = task_count,
                max = MAX_ESCALATIONS_PER_TASK,
                "Escalation cap reached for task — suppressing new escalation"
            );
            return (u64::MAX, None);
        }

        let mut next_id = self.next_id.write().await;
        let id = *next_id;
        *next_id += 1;
        drop(next_id);

        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(self.timeout_secs);
        let urgency_clone = urgency.clone();

        let escalation = PendingEscalation {
            id,
            task_id,
            agent_id,
            reason,
            context_summary,
            decision_point,
            options,
            urgency,
            blocking,
            trace_id,
            created_at: now,
            expires_at,
            auto_action: auto_action.unwrap_or(AutoAction::Deny),
            metadata,
            resolved: false,
            resolution: None,
            resolved_at: None,
        };

        escalations.push(escalation.clone());
        drop(escalations);
        // Install the resolution channel BEFORE dispatching sinks/webhooks
        // so a fast user resolve cannot land before the sender exists.
        let receiver = if install_resolution {
            let (tx, rx) = oneshot::channel();
            self.pending_resolution_tx.write().await.insert(id, tx);
            Some(rx)
        } else {
            None
        };

        // Audited only AFTER the resolution channel exists: `audit` is a
        // blocking SQLite write, and a resolve landing in that window would
        // find no sender and park the waiter until its own timeout.
        self.audit(Self::audit_entry(
            AuditEventType::EscalationCreated,
            &escalation,
            serde_json::json!({
                "escalation_id": id,
                "reason": format!("{:?}", escalation.reason),
                "urgency": escalation.urgency,
                "blocking": blocking,
                "decision_point": escalation.decision_point,
                "expires_at": expires_at.to_rfc3339(),
            }),
        ))
        .await;

        self.persist_escalation(escalation.clone()).await;
        tracing::info!(
            escalation_id = id,
            task_id = %task_id,
            expires_at = %expires_at.to_rfc3339(),
            "New escalation created"
        );

        self.fan_out_to_sinks(&escalation).await;

        // Wake the control panel now instead of on its next poll.
        self.emit_realtime("escalation.created", Some(id));

        // Fire-and-forget webhook notification if configured.
        // The URL is validated before use to prevent SSRF attacks.
        if let Some(url) = self.notify_url.read().await.clone() {
            match crate::network_safety::validate_webhook_url_str(&url) {
                Ok(()) => {
                    let payload = serde_json::json!({
                        "escalation_id": id,
                        "task_id": task_id.to_string(),
                        "agent_id": agent_id.to_string(),
                        "urgency": urgency_clone,
                        "blocking": blocking,
                        "expires_at": expires_at.to_rfc3339(),
                    });
                    tokio::spawn(async move {
                        let client = reqwest::Client::new();
                        if let Err(e) = client.post(&url).json(&payload).send().await {
                            tracing::warn!(
                                escalation_id = id,
                                error = %e,
                                "Failed to send escalation webhook notification"
                            );
                        }
                    });
                }
                Err(reason) => {
                    tracing::warn!(
                        escalation_id = id,
                        url = %url,
                        reason = %reason,
                        "Escalation webhook URL rejected — SSRF guard blocked the request"
                    );
                }
            }
        }

        (id, receiver)
    }

    /// List all pending (unresolved) escalations.
    pub async fn list_pending(&self) -> Vec<PendingEscalation> {
        self.escalations
            .read()
            .await
            .iter()
            .filter(|e| !e.resolved)
            .cloned()
            .collect()
    }

    /// Escalations an agent may reasonably still be asking about: everything
    /// unresolved, plus anything resolved within `resolved_grace`.
    ///
    /// `list_pending` alone is not enough behind the `escalation-status` tool.
    /// An agent polls precisely to learn whether its approval landed, and a
    /// pending-only view answers "no such escalation" the moment the operator
    /// says yes — indistinguishable from "never existed", which is how the
    /// 2026-09-08 agent concluded it was still blocked after three approvals.
    ///
    /// `list_all` is not the answer either: `prune_resolved` exists but is
    /// never scheduled, so the resolved tail grows for the life of the kernel
    /// and this runs on every tool call. The grace window keeps the clone
    /// bounded by the escalation *rate* rather than by uptime.
    pub async fn list_recent_for_agent(
        &self,
        agent_id: &AgentID,
        resolved_grace: chrono::Duration,
    ) -> Vec<PendingEscalation> {
        let cutoff = chrono::Utc::now() - resolved_grace;
        self.escalations
            .read()
            .await
            .iter()
            .filter(|e| e.agent_id == *agent_id)
            .filter(|e| match (e.resolved, e.resolved_at) {
                (false, _) => true,
                (true, Some(at)) => at >= cutoff,
                // Resolved but untimestamped: keep, the same defensive choice
                // `prune_resolved` makes.
                (true, None) => true,
            })
            .cloned()
            .collect()
    }

    /// List all escalations (including resolved).
    pub async fn list_all(&self) -> Vec<PendingEscalation> {
        self.escalations.read().await.clone()
    }

    /// Get a specific escalation by ID.
    pub async fn get(&self, id: u64) -> Option<PendingEscalation> {
        self.escalations
            .read()
            .await
            .iter()
            .find(|e| e.id == id)
            .cloned()
    }

    /// Resolve an escalation with a human decision.
    /// Returns the task_id, agent_id, and whether it was blocking.
    ///
    /// `resolution` is the operator-supplied decision string. It is normalized
    /// via [`resolution_is_approval`] so that CLI/API decisions like `"approve"`
    /// and channel decisions like `"approved"` (and `allow`/`allowed`, any case)
    /// all map to [`ResolutionOutcome::Approved`]. Anything else denies.
    pub async fn resolve(&self, id: u64, resolution: String) -> Option<(TaskID, AgentID, bool)> {
        let mut to_persist = None;
        let mut escalations = self.escalations.write().await;
        let result = if let Some(esc) = escalations.iter_mut().find(|e| e.id == id && !e.resolved) {
            esc.resolved = true;
            esc.resolution = Some(resolution.clone());
            esc.resolved_at = Some(chrono::Utc::now());
            let task_id = esc.task_id;
            let agent_id = esc.agent_id;
            let blocking = esc.blocking;
            to_persist = Some(esc.clone());
            tracing::info!(
                escalation_id = id,
                task_id = %task_id,
                "Escalation resolved"
            );
            Some((task_id, agent_id, blocking))
        } else {
            None
        };
        drop(escalations);

        // Computed once: the audit record, the device grant, and the outcome
        // sent on the resolution channel must never disagree.
        let mut approved = resolution_is_approval(&resolution);

        if let Some(escalation) = to_persist {
            // Grant first, for two reasons. An executor woken by the sender
            // below may retry the HAL call immediately and must not race the
            // grant; and if the grant fails there is nothing to approve, so
            // both the audit trail and the wake have to say so rather than
            // reporting a success that did not happen.
            let device_grant_failed = !self
                .apply_device_access_resolution(&escalation, approved)
                .await;
            if device_grant_failed {
                approved = false;
            }
            self.audit(Self::audit_entry(
                AuditEventType::EscalationResolved,
                &escalation,
                serde_json::json!({
                    "escalation_id": id,
                    "resolution": resolution,
                    "approved": approved,
                    "device_grant_failed": device_grant_failed,
                    "decision_point": escalation.decision_point,
                }),
            ))
            .await;
            self.persist_escalation(escalation).await;
        }

        // Wake the awaiting task_executor (if any). The receiver is
        // taken by `take_resolution_receiver` BEFORE the executor parks
        // on it, so a missing sender here just means nobody installed a
        // resolution channel for this escalation (e.g. escalation came
        // from a non-blocking source like CLI `agentos escalation
        // create`).
        if result.is_some() {
            let outcome = if approved {
                ResolutionOutcome::Approved
            } else {
                ResolutionOutcome::Denied
            };
            self.wake_resolution(id, outcome).await;
            // Drop the row from every open queue: a decision made on one
            // surface (channel DM, CLI) must not leave a live Approve button
            // on another.
            self.emit_realtime("escalation.resolved", Some(id));
        }

        result
    }

    /// Resolve all unresolved escalations attached to a task.
    ///
    /// Called when the owning task reaches a terminal state so escalations
    /// don't outlive their task and trigger spurious sweeper actions
    /// (auto-approve / auto-deny) after the task has already finished.
    /// Returns the number of escalations resolved.
    pub async fn resolve_for_task(&self, task_id: &TaskID, resolution: &str) -> usize {
        let now = chrono::Utc::now();
        let mut to_persist = Vec::new();
        {
            let mut escalations = self.escalations.write().await;
            for esc in escalations.iter_mut() {
                if esc.task_id == *task_id && !esc.resolved {
                    esc.resolved = true;
                    esc.resolution = Some(resolution.to_string());
                    esc.resolved_at = Some(now);
                    to_persist.push(esc.clone());
                }
            }
        }
        let count = to_persist.len();
        let outcome = if resolution_is_approval(resolution) {
            ResolutionOutcome::Approved
        } else {
            ResolutionOutcome::Denied
        };
        for escalation in to_persist {
            let id = escalation.id;
            self.persist_escalation(escalation).await;
            // A parked waiter on a task that just reached a terminal state
            // must be told, or it sits out its full approval window on a
            // decision that is already closed. Reachable whenever a second
            // caller deduped onto this task's device escalation
            // (`find_pending_device_access` matches on device+agent, not task).
            self.wake_resolution(id, outcome).await;
        }
        if count > 0 {
            tracing::info!(
                task_id = %task_id,
                count,
                resolution,
                "Resolved pending escalations for terminal task"
            );
            // The rows just vanished from the queue; refresh open panels. One
            // event for the batch: `to_persist` is already consumed and the
            // subscribers refetch the whole list regardless.
            self.emit_realtime("escalation.resolved", None);
        }
        count
    }

    /// Get escalations for a specific task.
    pub async fn for_task(&self, task_id: &TaskID) -> Vec<PendingEscalation> {
        self.escalations
            .read()
            .await
            .iter()
            .filter(|e| e.task_id == *task_id)
            .cloned()
            .collect()
    }

    /// Count pending escalations by urgency level.
    pub async fn pending_counts(&self) -> HashMap<String, usize> {
        let escalations = self.escalations.read().await;
        let mut counts = HashMap::new();
        for esc in escalations.iter().filter(|e| !e.resolved) {
            *counts.entry(esc.urgency.clone()).or_insert(0) += 1;
        }
        counts
    }

    /// Sweep expired escalations. Respects the `auto_action` field:
    /// - `AutoAction::Deny` → auto-deny (original behavior)
    /// - `AutoAction::Approve` → soft-approval (auto-approve on expiry)
    ///
    /// Returns `(id, task_id, agent_id, blocking, auto_action)` for each expired escalation.
    pub async fn sweep_expired(&self) -> Vec<(u64, TaskID, AgentID, bool, AutoAction)> {
        let now = chrono::Utc::now();
        let mut escalations = self.escalations.write().await;
        let mut expired = Vec::new();
        let mut to_persist = Vec::new();

        for esc in escalations.iter_mut() {
            if !esc.resolved && now >= esc.expires_at {
                esc.resolved = true;
                esc.resolved_at = Some(now);

                match esc.auto_action {
                    AutoAction::Approve => {
                        esc.resolution =
                            Some("Auto-approved: soft-approval window expired".to_string());
                        tracing::info!(
                            escalation_id = esc.id,
                            task_id = %esc.task_id,
                            "Escalation auto-approved (soft-approval)"
                        );
                    }
                    AutoAction::Deny => {
                        esc.resolution = Some("Auto-denied: escalation expired".to_string());
                        tracing::warn!(
                            escalation_id = esc.id,
                            task_id = %esc.task_id,
                            "Escalation auto-denied due to expiry"
                        );
                    }
                }

                expired.push((
                    esc.id,
                    esc.task_id,
                    esc.agent_id,
                    esc.blocking,
                    esc.auto_action,
                ));
                to_persist.push(esc.clone());
            }
        }
        drop(escalations);

        for escalation in to_persist {
            self.audit(Self::audit_entry(
                AuditEventType::EscalationExpired,
                &escalation,
                serde_json::json!({
                    "escalation_id": escalation.id,
                    "auto_action": format!("{:?}", escalation.auto_action).to_lowercase(),
                    "resolution": escalation.resolution,
                    "decision_point": escalation.decision_point,
                }),
            ))
            .await;
            self.persist_escalation(escalation).await;
        }

        // Wake any awaiting task_executor on expiry — they would
        // otherwise sit on the receiver until their own timeout fires.
        // Soft-approve maps to Approved, hard-deny maps to Denied. Errors
        // (receiver already dropped) are ignored.
        for (id, _, _, _, auto_action) in &expired {
            let outcome = match auto_action {
                AutoAction::Approve => ResolutionOutcome::Approved,
                AutoAction::Deny => ResolutionOutcome::Expired,
            };
            self.wake_resolution(*id, outcome).await;
        }

        if !expired.is_empty() {
            // One event for the whole sweep. Emitting per id turned an
            // away-from-desk backlog into N invalidations in a single tick,
            // each cancelling the refetch the last one started.
            self.emit_realtime("escalation.expired", None);
        }

        expired
    }

    /// Prune resolved escalations older than `max_age` from the in-memory list,
    /// and drop any orphaned resolution channels. Without this, a long-running
    /// kernel accumulates every resolved escalation in the in-memory `Vec` and
    /// leaks `pending_resolution_{tx,rx}` map entries forever. Full history is
    /// still retained in SQLite via `persist_escalation`, so dropping the
    /// in-memory copy of old resolved entries is safe. Returns the count pruned.
    pub async fn prune_resolved(&self, max_age: chrono::Duration) -> usize {
        let now = chrono::Utc::now();
        let mut escalations = self.escalations.write().await;
        let before = escalations.len();
        escalations.retain(|e| {
            if !e.resolved {
                return true;
            }
            match e.resolved_at {
                Some(at) => now - at < max_age,
                None => true, // resolved but untimestamped: keep (defensive)
            }
        });
        let pruned = before - escalations.len();
        // Any resolution channel whose escalation is no longer tracked is
        // orphaned — drop it so the maps don't grow unbounded.
        let live_ids: std::collections::HashSet<u64> = escalations.iter().map(|e| e.id).collect();
        drop(escalations);
        {
            let mut tx_map = self.pending_resolution_tx.write().await;
            tx_map.retain(|id, _| live_ids.contains(id));
            let mut rx_map = self.pending_resolution_rx.write().await;
            rx_map.retain(|id, _| live_ids.contains(id));
        }
        if pruned > 0 {
            tracing::debug!(pruned, "Pruned resolved escalations from in-memory list");
        }
        pruned
    }

    /// Create a soft-approval escalation with a 30-second auto-approve window.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_soft_approval(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        trace_id: TraceID,
    ) -> u64 {
        let mut next_id = self.next_id.write().await;
        let id = *next_id;
        *next_id += 1;

        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(30); // 30s soft-approval window

        let escalation = PendingEscalation {
            id,
            task_id,
            agent_id,
            reason,
            context_summary,
            decision_point,
            options,
            urgency: "normal".to_string(),
            blocking: false, // soft-approvals are non-blocking
            trace_id,
            created_at: now,
            expires_at,
            auto_action: AutoAction::Approve,
            metadata: default_metadata(),
            resolved: false,
            resolution: None,
            resolved_at: None,
        };

        self.escalations.write().await.push(escalation.clone());
        self.persist_escalation(escalation.clone()).await;
        tracing::info!(
            escalation_id = id,
            task_id = %task_id,
            expires_at = %expires_at.to_rfc3339(),
            "Soft-approval escalation created (auto-approves in 30s)"
        );
        self.emit_realtime("escalation.created", Some(id));

        id
    }

    pub async fn create_device_access_escalation(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        device_id: &str,
        operation: &str,
        trace_id: TraceID,
    ) -> (u64, bool) {
        if let Some(existing) = self.find_pending_device_access(device_id, &agent_id).await {
            return (existing.id, false);
        }

        let mut next_id = self.next_id.write().await;
        let id = *next_id;
        *next_id += 1;

        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(self.timeout_secs);
        let escalation = PendingEscalation {
            id,
            task_id,
            agent_id,
            reason: EscalationReason::AuthorizationRequired,
            context_summary: format!(
                "Agent requested access to hardware device '{}' for '{}' operation.",
                device_id, operation
            ),
            decision_point: format!("Approve HAL access to device '{}'", device_id),
            options: vec!["approve".to_string(), "deny".to_string()],
            urgency: "normal".to_string(),
            blocking: true,
            trace_id,
            created_at: now,
            expires_at,
            auto_action: AutoAction::Deny,
            metadata: serde_json::json!({
                "kind": "device_access",
                "device_id": device_id,
                "operation": operation,
            }),
            resolved: false,
            resolution: None,
            resolved_at: None,
        };

        self.escalations.write().await.push(escalation.clone());
        self.persist_escalation(escalation.clone()).await;
        tracing::info!(
            escalation_id = id,
            task_id = %task_id,
            device_id = %device_id,
            "HAL device access escalation created"
        );
        // This path bypasses `create_escalation_internal`, so nothing else
        // announces it — without this the whole hardware surface still lags the
        // panel by a poll interval while the agent sits blocked, and an
        // operator who is not looking at the panel never hears about it at all.
        self.fan_out_to_sinks(&escalation).await;
        self.emit_realtime("escalation.created", Some(id));

        (id, true)
    }

    pub async fn find_pending_device_access(
        &self,
        device_id: &str,
        agent_id: &AgentID,
    ) -> Option<PendingEscalation> {
        self.escalations
            .read()
            .await
            .iter()
            .find(|escalation| {
                !escalation.resolved
                    && escalation.agent_id == *agent_id
                    && escalation
                        .metadata
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        == Some("device_access")
                    && escalation
                        .metadata
                        .get("device_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(device_id)
            })
            .cloned()
    }

    /// Carry an operator decision on a `device_access` escalation through to
    /// the hardware registry.
    ///
    /// Without this the approval was recorded and dropped: `resolve` flipped
    /// the escalation to `approve` while the device stayed `Pending`, so the
    /// agent's next HAL call raised *another* escalation. The operator could
    /// approve indefinitely and never grant anything. The expiry path in
    /// `run_loop` has always carried the deny side through; this is the
    /// missing approve side.
    ///
    /// A denial deliberately leaves the device `Pending` rather than
    /// quarantining it — quarantine is irreversible from the agent's side and
    /// is a heavier consequence than "no, not this time". The expiry sweeper
    /// still quarantines on timeout.
    /// Returns `false` only when this *was* an approving device-access
    /// resolution and the grant did not take effect — the caller downgrades the
    /// decision so nothing reports a success that did not happen. Every other
    /// case (a denial, a non-device escalation) is `true`: there was nothing to
    /// grant, so nothing failed.
    ///
    /// The most reachable failure is a quarantined device: any device-access
    /// escalation that expires quarantines the device
    /// (`run_loop::sweep_expired`), and `approve_for_agent` refuses to approve
    /// a quarantined one. An operator whose first prompt timed out would
    /// otherwise see "resolved: approve" on every surface and watch nothing
    /// happen — the original symptom, one step later.
    ///
    /// ponytail: the grant lives only in the in-memory `HardwareRegistry`, so
    /// it does not survive a kernel restart; the escalation does, restored as
    /// resolved, so nothing replays it and the agent re-escalates. Persisting
    /// `granted_to` is the upgrade path if restarts start costing approvals.
    async fn apply_device_access_resolution(
        &self,
        escalation: &PendingEscalation,
        approved: bool,
    ) -> bool {
        if !approved {
            return true;
        }
        if escalation
            .metadata
            .get("kind")
            .and_then(serde_json::Value::as_str)
            != Some("device_access")
        {
            return true;
        }
        let Some(device_id) = escalation
            .metadata
            .get("device_id")
            .and_then(serde_json::Value::as_str)
        else {
            return true;
        };
        let Some(registry) = self.hardware_registry.read().await.clone() else {
            tracing::error!(
                escalation_id = escalation.id,
                device_id = %device_id,
                "Device access approved but no hardware registry is attached — grant dropped"
            );
            return false;
        };
        match registry.approve_for_agent(device_id, escalation.agent_id) {
            Ok(()) => {
                tracing::info!(
                    escalation_id = escalation.id,
                    device_id = %device_id,
                    agent_id = %escalation.agent_id,
                    "Device access granted from escalation approval"
                );
                true
            }
            Err(error) => {
                tracing::error!(
                    escalation_id = escalation.id,
                    device_id = %device_id,
                    agent_id = %escalation.agent_id,
                    error = %error,
                    "Device access approved but the registry grant failed"
                );
                false
            }
        }
    }

    pub async fn auto_resolve_device_escalation(
        &self,
        device_id: &str,
        agent_id: Option<&AgentID>,
        approved: bool,
    ) -> usize {
        let mut escalations = self.escalations.write().await;
        let now = chrono::Utc::now();
        let resolution = if approved {
            "Approved by operator"
        } else {
            "Denied by operator"
        };
        let mut updated = Vec::new();

        for escalation in escalations.iter_mut() {
            let is_device_access = escalation
                .metadata
                .get("kind")
                .and_then(serde_json::Value::as_str)
                == Some("device_access");
            let same_device = escalation
                .metadata
                .get("device_id")
                .and_then(serde_json::Value::as_str)
                == Some(device_id);
            let same_agent = agent_id
                .map(|expected| escalation.agent_id == *expected)
                .unwrap_or(true);

            if !escalation.resolved && is_device_access && same_device && same_agent {
                escalation.resolved = true;
                escalation.resolution = Some(resolution.to_string());
                escalation.resolved_at = Some(now);
                updated.push(escalation.clone());
            }
        }
        drop(escalations);

        let count = updated.len();
        let outcome = if approved {
            ResolutionOutcome::Approved
        } else {
            ResolutionOutcome::Denied
        };
        for escalation in updated {
            let id = escalation.id;
            self.persist_escalation(escalation).await;
            // Wake anyone parked in `KernelDeviceAccessGate::check`. The
            // caller has already applied the grant to the hardware registry,
            // so the woken call re-checks and proceeds. Without this an
            // `agentos hal approve` (or the HAL page) left the agent parked
            // until its approval window expired, even though the device was
            // approved seconds after it asked.
            self.wake_resolution(id, outcome).await;
        }
        if count > 0 {
            // Approving a device from the HAL page or the CLI silently flips
            // these rows. Without the push, the queue keeps rendering Approve
            // for them and the click comes back a 409 on a decision the
            // operator already made.
            self.emit_realtime("escalation.resolved", None);
        }

        count
    }
}

impl Default for EscalationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_create_and_list_escalation() {
        let manager = EscalationManager::new();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();

        let id = manager
            .create_escalation(
                task_id,
                agent_id,
                EscalationReason::Uncertainty,
                "Agent unsure about file deletion".to_string(),
                "Should I delete /data/old_reports?".to_string(),
                vec!["Yes, delete".to_string(), "No, keep".to_string()],
                "normal".to_string(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        assert_eq!(id, 1);
        let pending = manager.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].task_id, task_id);
        assert!(pending[0].blocking);
    }

    /// Mock sink that records every broadcast call. Used by the
    /// integration tests below to verify that `EscalationManager`
    /// fans new escalations out to every registered sink, that the
    /// 30s timeout wrap (R3 finding I3) does not interfere with
    /// well-behaved sinks, and that resolved/idempotent paths do not
    /// emit duplicates.
    struct RecordingSink {
        seen: Arc<tokio::sync::Mutex<Vec<u64>>>,
    }

    #[async_trait::async_trait]
    impl BroadcastSink for RecordingSink {
        async fn broadcast(&self, escalation: &PendingEscalation) {
            self.seen.lock().await.push(escalation.id);
        }
        fn name(&self) -> &'static str {
            "test-recording"
        }
    }

    /// Sink that hangs forever — verifies that the timeout wrap added
    /// in R3 finding I3 actually bounds the detached spawn.
    struct HangingSink;

    #[async_trait::async_trait]
    impl BroadcastSink for HangingSink {
        async fn broadcast(&self, _escalation: &PendingEscalation) {
            std::future::pending::<()>().await;
        }
        fn name(&self) -> &'static str {
            "test-hanging"
        }
    }

    /// The panel's approval queue is poll-driven; without this push it lags the
    /// operator's phone notification by a poll interval. Covers both ends of an
    /// escalation's life, since a stale Approve button is as bad as a missing one.
    #[tokio::test]
    async fn realtime_sender_receives_create_and_resolve() {
        let manager = EscalationManager::new();
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        manager.set_realtime_sender(tx);

        let id = manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "ctx".into(),
                "decide".into(),
                vec!["approve".into()],
                "high".into(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        let created = rx.try_recv().expect("create should broadcast");
        assert_eq!(created.channel, "escalations");
        assert_eq!(created.event, "escalation.created");
        assert_eq!(created.data["escalation_id"], id);
        // Only the id travels — the escalation body can carry sensitive args.
        assert_eq!(created.data.as_object().unwrap().len(), 1);

        manager.resolve(id, "approve".to_string()).await;
        let resolved = rx.try_recv().expect("resolve should broadcast");
        assert_eq!(resolved.event, "escalation.resolved");
        assert_eq!(resolved.data["escalation_id"], id);
    }

    #[tokio::test]
    async fn create_escalation_fans_out_to_registered_sinks() {
        let manager = EscalationManager::new();
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        manager
            .add_sink(Arc::new(RecordingSink { seen: seen.clone() }))
            .await;

        let id = manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "ctx".into(),
                "decision".into(),
                vec!["yes".into(), "no".into()],
                "high".into(),
                true,
                TraceID::new(),
                None,
            )
            .await;
        assert_ne!(id, u64::MAX);

        // Sinks run in spawned tasks — give them a moment to land.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let recorded = seen.lock().await.clone();
        assert_eq!(
            recorded,
            vec![id],
            "sink should observe exactly one broadcast"
        );
    }

    #[tokio::test]
    async fn create_escalation_does_not_block_on_hanging_sink() {
        let manager = EscalationManager::new();
        manager.add_sink(Arc::new(HangingSink)).await;

        let start = std::time::Instant::now();
        let _ = manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "ctx".into(),
                "decision".into(),
                vec![],
                "high".into(),
                true,
                TraceID::new(),
                None,
            )
            .await;
        // Even with a sink that hangs forever, create_escalation must
        // return promptly because broadcasts are detached spawns.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "create_escalation took {:?} (sink fan-out is supposed to be detached)",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn resolve_wakes_pending_resolution_receiver_with_approved() {
        let manager = Arc::new(EscalationManager::new());
        let id = manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "summary".into(),
                "decision".into(),
                vec![],
                "high".into(),
                true,
                TraceID::new(),
                None,
            )
            .await;
        manager.prepare_resolution(id).await;

        // The receiver belongs to whoever calls `take_resolution_receiver` first.
        let rx = manager.take_resolution_receiver(id).await.expect("rx");

        // Spawn the resolve concurrently so we exercise the wake path.
        let mgr = Arc::clone(&manager);
        let waker = tokio::spawn(async move { mgr.resolve(id, "approved".into()).await });
        let outcome = rx.await.expect("sender not dropped");
        assert!(matches!(outcome, ResolutionOutcome::Approved));
        let resolved = waker.await.unwrap();
        assert!(resolved.is_some());
    }

    #[tokio::test]
    async fn resolve_wakes_with_denied_for_non_approved_resolution() {
        let manager = Arc::new(EscalationManager::new());
        let id = manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "summary".into(),
                "decision".into(),
                vec![],
                "high".into(),
                true,
                TraceID::new(),
                None,
            )
            .await;
        manager.prepare_resolution(id).await;
        let rx = manager.take_resolution_receiver(id).await.unwrap();
        manager.resolve(id, "denied".into()).await;
        let outcome = rx.await.unwrap();
        assert!(matches!(outcome, ResolutionOutcome::Denied));
    }

    #[tokio::test]
    async fn sweep_expired_wakes_with_auto_action_outcome() {
        // Manager with zero timeout so create + sweep round-trip is instant.
        let manager = EscalationManager {
            escalations: RwLock::new(Vec::new()),
            next_id: RwLock::new(1),
            timeout_secs: 0,
            notify_url: RwLock::new(None),
            state_store: None,
            broadcast_sinks: RwLock::new(Vec::new()),
            pending_resolution_tx: RwLock::new(HashMap::new()),
            audit: RwLock::new(None),
            pending_resolution_rx: RwLock::new(HashMap::new()),
            hardware_registry: RwLock::new(None),
            realtime_tx: std::sync::OnceLock::new(),
        };
        let id = manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "summary".into(),
                "decision".into(),
                vec![],
                "high".into(),
                true,
                TraceID::new(),
                Some(AutoAction::Deny),
            )
            .await;
        manager.prepare_resolution(id).await;
        let rx = manager.take_resolution_receiver(id).await.unwrap();
        let expired = manager.sweep_expired().await;
        assert_eq!(expired.len(), 1);
        let outcome = rx.await.unwrap();
        assert!(matches!(outcome, ResolutionOutcome::Expired));
    }

    #[tokio::test]
    async fn take_resolution_receiver_returns_none_when_not_prepared() {
        let manager = EscalationManager::new();
        assert!(manager.take_resolution_receiver(999).await.is_none());
    }

    /// End-to-end shape test for the watchdog user-gate. Mirrors the
    /// 3-way `tokio::select!` in `task_executor.rs` (long-running
    /// future vs resolution receiver vs grace timer). Verifies that
    /// an Approved resolution wakes ahead of the grace timer, an
    /// Abort wakes before the slow future, and a slow future without
    /// resolution falls through to grace. This protects against
    /// future regressions that would silently desync the gate from
    /// the resolution channel.
    // Watchdog user-gate select! shape tests. Real wall-clock with
    // millisecond durations — same control flow as the production
    // 120s/60s timers, just compressed for fast unit testing. The
    // tokio test-util `time::pause` API is unavailable on this build,
    // so we use realistic-but-tiny waits (≤10 ms).
    #[tokio::test]
    async fn user_gate_resolves_approved_ahead_of_grace_and_inference() {
        let manager = Arc::new(EscalationManager::new());
        let (id, rx) = manager
            .create_escalation_with_resolution(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::Other("long_running_inference".into()),
                "summary".into(),
                "decision".into(),
                vec!["Continue".into(), "Abort".into()],
                "high".into(),
                true,
                TraceID::new(),
                Some(AutoAction::Deny),
            )
            .await;
        let rx = rx.expect("rx installed");

        let slow_inference = tokio::time::sleep(std::time::Duration::from_secs(5));
        let grace = tokio::time::sleep(std::time::Duration::from_secs(2));
        tokio::pin!(slow_inference);
        tokio::pin!(grace);

        let mgr = Arc::clone(&manager);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            mgr.resolve(id, "approved".into()).await
        });

        let outcome: Result<ResolutionOutcome, &'static str> = tokio::select! {
            biased;
            _ = &mut slow_inference => Err("slow_inference stole the gate"),
            r = rx => r.map_err(|_| "rx dropped"),
            _ = &mut grace => Err("grace stole the gate"),
        };
        assert_eq!(outcome.unwrap(), ResolutionOutcome::Approved);
    }

    #[tokio::test]
    async fn user_gate_abort_wakes_before_inference() {
        let manager = Arc::new(EscalationManager::new());
        let (id, rx) = manager
            .create_escalation_with_resolution(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::Other("long_running_inference".into()),
                "summary".into(),
                "decision".into(),
                vec![],
                "high".into(),
                true,
                TraceID::new(),
                Some(AutoAction::Deny),
            )
            .await;
        let rx = rx.expect("rx installed");
        let slow_inference = tokio::time::sleep(std::time::Duration::from_secs(5));
        let grace = tokio::time::sleep(std::time::Duration::from_secs(2));
        tokio::pin!(slow_inference);
        tokio::pin!(grace);

        let mgr = Arc::clone(&manager);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            mgr.resolve(id, "denied".into()).await
        });

        let outcome: ResolutionOutcome = tokio::select! {
            biased;
            _ = &mut slow_inference => panic!("inference should not finish"),
            r = rx => r.expect("sender delivered"),
            _ = &mut grace => panic!("grace should not fire"),
        };
        assert_eq!(outcome, ResolutionOutcome::Denied);
    }

    #[tokio::test]
    async fn user_gate_grace_falls_through_when_no_resolution() {
        let slow_inference = tokio::time::sleep(std::time::Duration::from_secs(5));
        let grace = tokio::time::sleep(std::time::Duration::from_millis(200));
        tokio::pin!(slow_inference);
        tokio::pin!(grace);

        let fired_grace: bool = tokio::select! {
            biased;
            _ = &mut slow_inference => false,
            _ = &mut grace => true,
        };
        assert!(
            fired_grace,
            "grace timer must fire when no resolution arrives"
        );
    }

    /// Race regression: a `resolve()` that lands the instant the
    /// escalation is created (before the caller even awaits the
    /// receiver) must still wake the awaiter. Closes the window where
    /// `create_escalation` returned but `prepare_resolution` had not
    /// yet been called — fixed by
    /// `create_escalation_with_resolution`, which installs the
    /// oneshot pair atomically before any broadcasts/webhooks fire.
    #[tokio::test]
    async fn create_with_resolution_survives_immediate_resolve() {
        let manager = Arc::new(EscalationManager::new());
        let (id, rx) = manager
            .create_escalation_with_resolution(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "summary".into(),
                "decision".into(),
                vec![],
                "high".into(),
                true,
                TraceID::new(),
                None,
            )
            .await;
        let rx = rx.expect("receiver installed atomically");

        // Resolve immediately; awaiter has not yet polled rx.
        let resolved = manager.resolve(id, "approved".into()).await;
        assert!(resolved.is_some(), "resolve found the escalation");

        // Awaiter must still observe the wake — proves the sender was
        // installed before any external resolver could race in.
        let outcome = rx.await.expect("sender delivered");
        assert!(matches!(outcome, ResolutionOutcome::Approved));
    }

    #[tokio::test]
    async fn test_resolve_escalation() {
        let manager = EscalationManager::new();
        let task_id = TaskID::new();

        let id = manager
            .create_escalation(
                task_id,
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "summary".to_string(),
                "decision".to_string(),
                vec![],
                "high".to_string(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        let result = manager.resolve(id, "Approved by admin".to_string()).await;
        assert!(result.is_some());
        let (resolved_task_id, resolved_agent_id, blocking) = result.unwrap();
        assert_eq!(resolved_task_id, task_id);
        assert_eq!(resolved_agent_id, manager.list_all().await[0].agent_id);
        assert!(blocking);

        // Should no longer appear in pending
        assert!(manager.list_pending().await.is_empty());
        // But should still be in all
        assert_eq!(manager.list_all().await.len(), 1);
    }

    #[tokio::test]
    async fn test_resolve_nonexistent_returns_none() {
        let manager = EscalationManager::new();
        assert!(manager.resolve(999, "nope".to_string()).await.is_none());
    }

    #[tokio::test]
    async fn test_pending_counts() {
        let manager = EscalationManager::new();

        for urgency in &["normal", "normal", "high", "critical"] {
            manager
                .create_escalation(
                    TaskID::new(),
                    AgentID::new(),
                    EscalationReason::Uncertainty,
                    "s".to_string(),
                    "d".to_string(),
                    vec![],
                    urgency.to_string(),
                    false,
                    TraceID::new(),
                    None,
                )
                .await;
        }

        let counts = manager.pending_counts().await;
        assert_eq!(counts.get("normal"), Some(&2));
        assert_eq!(counts.get("high"), Some(&1));
        assert_eq!(counts.get("critical"), Some(&1));
    }

    #[tokio::test]
    async fn test_sweep_expired_auto_denies() {
        let manager = EscalationManager {
            escalations: RwLock::new(Vec::new()),
            next_id: RwLock::new(1),
            timeout_secs: 0, // expire immediately
            notify_url: RwLock::new(None),
            state_store: None,
            broadcast_sinks: RwLock::new(Vec::new()),
            pending_resolution_tx: RwLock::new(HashMap::new()),
            audit: RwLock::new(None),
            pending_resolution_rx: RwLock::new(HashMap::new()),
            hardware_registry: RwLock::new(None),
            realtime_tx: std::sync::OnceLock::new(),
        };

        let task_id = TaskID::new();
        manager
            .create_escalation(
                task_id,
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "test".to_string(),
                "test".to_string(),
                vec![],
                "high".to_string(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        // Sweep should auto-deny the expired escalation
        let expired = manager.sweep_expired().await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, 1); // id
        assert_eq!(expired[0].1, task_id);
        assert_eq!(expired[0].2, manager.list_all().await[0].agent_id);
        assert!(expired[0].3); // blocking
        assert_eq!(expired[0].4, AutoAction::Deny);

        // Should no longer appear in pending
        assert!(manager.list_pending().await.is_empty());

        // Resolution should indicate auto-deny
        let all = manager.list_all().await;
        assert!(all[0].resolution.as_ref().unwrap().contains("Auto-denied"));
    }

    #[tokio::test]
    async fn test_sweep_expired_auto_approves() {
        let manager = EscalationManager {
            escalations: RwLock::new(Vec::new()),
            next_id: RwLock::new(1),
            timeout_secs: 0, // expire immediately
            notify_url: RwLock::new(None),
            state_store: None,
            broadcast_sinks: RwLock::new(Vec::new()),
            pending_resolution_tx: RwLock::new(HashMap::new()),
            audit: RwLock::new(None),
            pending_resolution_rx: RwLock::new(HashMap::new()),
            hardware_registry: RwLock::new(None),
            realtime_tx: std::sync::OnceLock::new(),
        };

        let task_id = TaskID::new();
        manager
            .create_escalation(
                task_id,
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "test".to_string(),
                "test".to_string(),
                vec![],
                "normal".to_string(),
                true,
                TraceID::new(),
                Some(AutoAction::Approve),
            )
            .await;

        let expired = manager.sweep_expired().await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, 1); // id
        assert_eq!(expired[0].1, task_id);
        assert_eq!(expired[0].2, manager.list_all().await[0].agent_id);
        assert!(expired[0].3); // blocking
        assert_eq!(expired[0].4, AutoAction::Approve);

        assert!(manager.list_pending().await.is_empty());

        let all = manager.list_all().await;
        assert!(all[0]
            .resolution
            .as_ref()
            .unwrap()
            .contains("Auto-approved"));
    }

    #[tokio::test]
    async fn test_restore_from_store_recovers_unresolved_escalations() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("kernel_state.db");
        let store = Arc::new(
            KernelStateStore::open(db_path)
                .await
                .expect("state store should open"),
        );

        let manager = EscalationManager::with_state_store(Some(store.clone()));
        let task_id = TaskID::new();
        let agent_id = AgentID::new();

        let unresolved_id = manager
            .create_escalation(
                task_id,
                agent_id,
                EscalationReason::AuthorizationRequired,
                "needs review".to_string(),
                "approve?".to_string(),
                vec!["yes".to_string(), "no".to_string()],
                "high".to_string(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        let resolved_id = manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::Uncertainty,
                "second".to_string(),
                "resolve".to_string(),
                vec![],
                "normal".to_string(),
                false,
                TraceID::new(),
                None,
            )
            .await;
        manager
            .resolve(resolved_id, "approved".to_string())
            .await
            .expect("resolution should succeed");

        let restored_manager = EscalationManager::with_state_store(Some(store));
        let restored = restored_manager
            .restore_from_store()
            .await
            .expect("restore should succeed");
        assert_eq!(restored, 1, "only unresolved escalation should be restored");

        let pending = restored_manager.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, unresolved_id);

        // Ensure next ID continues after previously persisted rows.
        let next = restored_manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::Uncertainty,
                "new".to_string(),
                "new".to_string(),
                vec![],
                "normal".to_string(),
                false,
                TraceID::new(),
                None,
            )
            .await;
        assert!(next > resolved_id);
    }

    #[tokio::test]
    async fn test_create_device_access_escalation_deduplicates_by_device_and_agent() {
        let manager = EscalationManager::new();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();

        let (first_id, created_first) = manager
            .create_device_access_escalation(task_id, agent_id, "gpu:0", "read", TraceID::new())
            .await;
        let (second_id, created_second) = manager
            .create_device_access_escalation(task_id, agent_id, "gpu:0", "read", TraceID::new())
            .await;

        assert!(created_first);
        assert!(!created_second);
        assert_eq!(first_id, second_id);
        assert_eq!(manager.list_pending().await.len(), 1);
        assert_eq!(
            manager.list_pending().await[0].metadata["kind"].as_str(),
            Some("device_access")
        );
    }

    /// A HAL device prompt must reach the operator's channels, not just the
    /// control panel. `create_device_access_escalation` bypasses
    /// `create_escalation_internal`, so it announced itself on the panel SSE
    /// stream and nowhere else — an operator working over Telegram was never
    /// told the agent was parked waiting for them.
    #[tokio::test]
    async fn device_access_escalation_fans_out_to_registered_sinks() {
        let manager = EscalationManager::new();
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        manager
            .add_sink(Arc::new(RecordingSink { seen: seen.clone() }))
            .await;

        let (id, created) = manager
            .create_device_access_escalation(
                TaskID::new(),
                AgentID::new(),
                "audio:49",
                "write",
                TraceID::new(),
            )
            .await;
        assert!(created);

        // Sinks run in spawned tasks — give them a moment to land.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(seen.lock().await.clone(), vec![id]);
    }

    /// Every path that resolves an escalation must deliver the outcome to a
    /// parked waiter. `auto_resolve_device_escalation` (the `agentos hal
    /// approve` path) and `resolve_for_task` (terminal task cleanup) both
    /// recorded the decision and dropped it on the floor, leaving the waiter
    /// parked until its own timeout.
    #[tokio::test]
    async fn resolution_paths_wake_a_parked_waiter() {
        for approve in [true, false] {
            let manager = EscalationManager::new();
            let agent_id = AgentID::new();
            let (id, created) = manager
                .create_device_access_escalation(
                    TaskID::new(),
                    agent_id,
                    "audio:49",
                    "write",
                    TraceID::new(),
                )
                .await;
            assert!(created);
            manager.prepare_resolution(id).await;
            let rx = manager
                .take_resolution_receiver(id)
                .await
                .expect("channel installed");

            manager
                .auto_resolve_device_escalation("audio:49", Some(&agent_id), approve)
                .await;

            let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), rx)
                .await
                .expect("operator device decision must wake the waiter, not time it out")
                .expect("sender must not be dropped without an outcome");
            assert_eq!(
                outcome,
                if approve {
                    ResolutionOutcome::Approved
                } else {
                    ResolutionOutcome::Denied
                }
            );
        }
    }

    #[tokio::test]
    async fn resolve_for_task_wakes_a_parked_waiter() {
        let manager = EscalationManager::new();
        let task_id = TaskID::new();
        let (id, _) = manager
            .create_device_access_escalation(
                task_id,
                AgentID::new(),
                "webcam:video0",
                "execute",
                TraceID::new(),
            )
            .await;
        manager.prepare_resolution(id).await;
        let rx = manager
            .take_resolution_receiver(id)
            .await
            .expect("channel installed");

        // A second caller can dedup onto this escalation and park on it, so a
        // task reaching a terminal state has to release it.
        manager.resolve_for_task(&task_id, "Task terminated").await;

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .expect("terminal-task cleanup must wake the waiter")
            .expect("sender must not be dropped without an outcome");
        assert_eq!(outcome, ResolutionOutcome::Denied);
    }

    #[tokio::test]
    async fn test_auto_resolve_device_escalation_matches_device_and_agent() {
        let manager = EscalationManager::new();
        let allowed_agent = AgentID::new();
        let other_agent = AgentID::new();

        manager
            .create_device_access_escalation(
                TaskID::new(),
                allowed_agent,
                "sensor:thermal_zone0",
                "read",
                TraceID::new(),
            )
            .await;
        manager
            .create_device_access_escalation(
                TaskID::new(),
                other_agent,
                "sensor:thermal_zone0",
                "read",
                TraceID::new(),
            )
            .await;

        let resolved = manager
            .auto_resolve_device_escalation("sensor:thermal_zone0", Some(&allowed_agent), true)
            .await;

        assert_eq!(resolved, 1);
        let pending = manager.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].agent_id, other_agent);
    }
}
