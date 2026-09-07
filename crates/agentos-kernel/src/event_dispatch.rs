use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_capability::CapabilityEngine;
use agentos_types::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use crate::kernel::Kernel;

/// The agent whose own activity caused this event, if identifiable from the
/// payload. Used by the trigger-loop guard in `process_event`: that agent's
/// subscriptions must not spawn a reaction task for its own activity.
///
/// Emit sites use different payload keys for the causer; first match wins:
/// `agent_id` (task lifecycle), `preempted_agent` (TaskPreempted),
/// `blocked_agent` (TaskDeadlockDetected), `from_agent` (direct messages /
/// broadcasts — the sender, never the recipient), `delegating_agent_id`
/// (DelegationReceived), `child_agent_id` (DelegationResponseReceived).
/// TaskLifecycle emit sites MUST carry one of these keys or the guard no-ops.
fn causing_agent(payload: &serde_json::Value) -> Option<AgentID> {
    [
        "agent_id",
        "preempted_agent",
        "blocked_agent",
        "from_agent",
        "delegating_agent_id",
        "child_agent_id",
    ]
    .iter()
    .find_map(|k| payload.get(*k)?.as_str()?.parse().ok())
}

/// Whether an event of this type must not spawn a reaction task on the agent
/// that caused it. See the trigger-loop guard in `process_event` for why.
///
/// Matched by TYPE for `AgentAdded`, by CATEGORY for the families whose events
/// are driven by an agent's own task activity. The budget pair is NOT listed
/// here: it never spawns a reaction task for *anyone* (see [`is_budget_event`]),
/// which subsumes the self-exclusion it used to need.
fn is_self_excludable(event_type: EventType) -> bool {
    matches!(event_type, EventType::AgentAdded)
        || matches!(
            event_type.category(),
            EventCategory::TaskLifecycle
                | EventCategory::AgentCommunication
                | EventCategory::MemoryEvents
                | EventCategory::ToolEvents
                | EventCategory::SecurityEvents
        )
}

/// Budget events notify, they never trigger.
///
/// `BudgetWarning`/`BudgetExhausted` mean "this agent has no money left"; a
/// reaction task for such an agent can only trip the budget again on its first
/// inference, and a self-feeding chain is exactly what took 148 tasks/80min on
/// 2026-08-31. Matched by TYPE, not by their `SystemHealth` category — the rest
/// of that category (`ProcessCrashed`, `MemoryPressure`, ...) still triggers.
fn is_budget_event(event_type: EventType) -> bool {
    matches!(
        event_type,
        EventType::BudgetWarning | EventType::BudgetExhausted
    )
}

/// Whether a reaction must be spawned now instead of waiting for the batch
/// window: `Critical` subscriptions asked for immediacy, and inter-agent
/// messages are a conversation — a coalesced digest would break the turn.
fn bypasses_batching(priority: SubscriptionPriority, category: EventCategory) -> bool {
    priority == SubscriptionPriority::Critical || category == EventCategory::AgentCommunication
}

/// Per-agent coalescing buffer for event reactions.
///
/// Without it a burst of N matching events spawns N tasks for the same agent,
/// each paying a full inference. Reactions land in `pending` for at most
/// `kernel.events.reaction_batch_window_secs`, then flush as ONE task carrying
/// a digest of the batch. `in_flight` holds the task a flush produced so the
/// next window waits for it instead of stacking a second task on the same
/// agent; it is cleared from the terminal task paths (see
/// `Kernel::drain_reactions_after_task`) and, when those are bypassed, by the
/// scheduler liveness check in [`Kernel::flush_reactions`].
///
/// The slot value is `None` while a flush is between reserving the slot and
/// finishing its spawn: reserving before the `pending` lock is taken is what
/// stops two concurrent flushes (the window timer and the max-events early
/// flush) from both seeing an empty map and both spawning a task.
/// Per-agent in-flight reaction slot: the task id once spawned (`None` while a
/// flush holds the reservation) and when the slot was taken.
type InFlightSlot = (Option<TaskID>, chrono::DateTime<chrono::Utc>);

#[derive(Default)]
pub(crate) struct ReactionBatcher {
    pending: tokio::sync::Mutex<HashMap<AgentID, PendingBatch>>,
    in_flight: tokio::sync::Mutex<HashMap<AgentID, InFlightSlot>>,
}

#[derive(Default)]
struct PendingBatch {
    items: Vec<(EventSubscription, EventMessage)>,
    /// A flush timer is already armed for this agent — don't arm a second one.
    timer_started: bool,
    /// Events dropped from the digest by the `reaction_batch_max_events` cap.
    /// They are in the inbox; the digest header says how many.
    overflow: usize,
}

/// Subscription priority as a scheduler task priority — HIGHER is more urgent.
/// The scheduler's heap dequeues the largest number first (`Ord for
/// PrioritizedTask` in scheduler.rs); every other producer defaults to 5 and
/// webhooks use 7. The inline match this replaced had `Critical => 1`, which
/// ran Critical reactions *after* every chat task and every Low reaction.
fn priority_rank(priority: SubscriptionPriority) -> u8 {
    match priority {
        SubscriptionPriority::Critical => 9,
        SubscriptionPriority::High => 7,
        SubscriptionPriority::Normal => 5,
        SubscriptionPriority::Low => 3,
    }
}

/// Cap on `event_ids` in a batched `EventTriggeredTask` audit entry — the
/// details column has a 64 KiB ceiling.
const MAX_AUDITED_EVENT_IDS: usize = 200;

/// An in-flight reaction task older than this is presumed dead (kernel restart,
/// panic before the terminal path ran) and no longer blocks a flush.
const IN_FLIGHT_STALE_HOURS: i64 = 2;

/// Byte ceiling for a batch digest prompt.
const MAX_DIGEST_BYTES: usize = 4096;

/// Distinct values seen for one top-level payload key across a type group.
#[derive(Default)]
struct KeySummary {
    /// First-seen order, deduplicated. Bounded by the batch size, which is
    /// bounded by `kernel.events.reaction_batch_max_events`.
    distinct: Vec<String>,
    /// How many payloads in the group carried this key.
    present: usize,
}

/// Render a scalar payload value, or `None` for objects/arrays/null — the full
/// payloads are already in the agent's inbox, the digest only orients it.
fn scalar_text(value: &serde_json::Value) -> Option<String> {
    let text = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => return None,
    };
    // ponytail: one pathological value must not eat the whole byte budget.
    if text.chars().count() > 60 {
        Some(format!("{}…", text.chars().take(60).collect::<String>()))
    } else {
        Some(text)
    }
}

/// Summarize the top-level scalar keys of a group's payloads, keys in
/// first-seen order.
fn scalar_key_summaries(payloads: &[&serde_json::Value]) -> Vec<(String, KeySummary)> {
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, KeySummary> = HashMap::new();
    for payload in payloads {
        let Some(obj) = payload.as_object() else {
            continue;
        };
        for (key, value) in obj {
            let Some(text) = scalar_text(value) else {
                continue;
            };
            let entry = map.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                KeySummary::default()
            });
            entry.present += 1;
            if !entry.distinct.iter().any(|d| d == &text) {
                entry.distinct.push(text);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|k| map.remove(&k).map(|s| (k, s)))
        .collect()
}

/// Truncate to at most `max` bytes without splitting a UTF-8 code point.
fn clamp_bytes(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}

/// Build the digest prompt for a coalesced batch.
///
/// Deterministic: event types keep first-seen order, keys keep first-seen order
/// within their type. `max_bytes` is enforced by a sample ladder (10 → 3 → 0
/// values per key) and, as a last resort, a char-boundary-safe truncation.
fn build_batch_digest(
    items: &[(EventSubscription, EventMessage)],
    overflow: usize,
    max_bytes: usize,
) -> String {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&serde_json::Value>> = HashMap::new();
    for (_, event) in items {
        let name = format!("{:?}", event.event_type);
        let group = groups.entry(name.clone()).or_insert_with(|| {
            order.push(name);
            Vec::new()
        });
        group.push(&event.payload);
    }

    // Events past `reaction_batch_max_events` never reach `items`; without
    // this the header claims "200 events" when 5000 arrived.
    let overflow_note = if overflow > 0 {
        format!(" (+{overflow} more in your inbox)")
    } else {
        String::new()
    };
    let render = |samples: usize| -> String {
        let mut out = format!(
            "[EVENT BATCH] {} events coalesced within one window{}. Full payloads are in your inbox.\n",
            items.len(),
            overflow_note
        );
        for name in &order {
            let Some(payloads) = groups.get(name) else {
                continue;
            };
            out.push_str(&format!("{name} x{}\n", payloads.len()));
            if samples == 0 {
                continue;
            }
            for (key, summary) in scalar_key_summaries(payloads) {
                match summary.distinct.as_slice() {
                    [only] if summary.present == payloads.len() => {
                        out.push_str(&format!("  {key}={only}\n"));
                    }
                    distinct => {
                        let shown = distinct.len().min(samples);
                        let list = distinct[..shown].join(", ");
                        let more = distinct.len() - shown;
                        if more > 0 {
                            out.push_str(&format!("  {key}: {list} (+{more} more)\n"));
                        } else {
                            out.push_str(&format!("  {key}: {list}\n"));
                        }
                    }
                }
            }
        }
        out
    };

    for samples in [10usize, 3, 0] {
        let out = render(samples);
        if out.len() <= max_bytes {
            return out;
        }
    }
    clamp_bytes(render(0), max_bytes)
}

/// Map a kernel [`EventMessage`] to a coarse [`RealtimeEvent`] for WS/SSE fan-out.
///
/// The channel is derived from the event's category so control-panel clients can
/// subscribe by domain (`tasks`, `agents`, `audit`, `schedules`, `system`, or the
/// catch-all `events`). The event name is the variant name; `data` carries the
/// type, severity, and timestamp — plus the payload for non-sensitive categories
/// (security/tool payloads are withheld; see below).
fn realtime_event_from(event: &EventMessage) -> RealtimeEvent {
    let category = event.event_type.category();
    // Chat has its own category *and* channel: the panel invalidates a session
    // transcript on it, and an agent subscribed to `AgentCommunication` must
    // never be woken by chat traffic (its reply would emit another chat event).
    let channel = match category {
        EventCategory::ChatEvents => "chat",
        EventCategory::AgentLifecycle => "agents",
        EventCategory::TaskLifecycle => "tasks",
        EventCategory::SecurityEvents | EventCategory::ToolEvents => "audit",
        EventCategory::ScheduleEvents => "schedules",
        EventCategory::SystemHealth => "system",
        EventCategory::MemoryEvents
        | EventCategory::HardwareEvents
        | EventCategory::AgentCommunication
        | EventCategory::ExternalEvents => "events",
    };
    // Security/tool event payloads may embed secret names or raw tool arguments
    // (which can carry tokens). Do NOT broadcast those raw payloads to every
    // `audit:r` subscriber — forward only the non-sensitive envelope. The full
    // payload remains in the signed, access-controlled audit log.
    let include_payload = !matches!(
        category,
        EventCategory::SecurityEvents | EventCategory::ToolEvents
    );
    let mut data = serde_json::json!({
        "event_id": event.id.to_string(),
        "event_type": format!("{:?}", event.event_type),
        "severity": format!("{:?}", event.severity),
        "timestamp": event.timestamp.to_rfc3339(),
    });
    if include_payload {
        if let Some(obj) = data.as_object_mut() {
            obj.insert("payload".to_string(), event.payload.clone());
        }
    }
    RealtimeEvent {
        channel: channel.to_string(),
        event: format!("{:?}", event.event_type),
        data,
    }
}

/// Sign an event, write an audit entry, and send it through the event channel.
///
/// This is the single authoritative implementation of event emission.  Both
/// `Kernel::emit_event_with_trace` (which has `&self`) and spawned background
/// tasks (which only hold cloned `Arc` handles) call this function, ensuring
/// the HMAC canonical format and audit schema stay in sync.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_signed_event(
    capability_engine: &CapabilityEngine,
    audit: &AuditLog,
    event_sender: &tokio::sync::mpsc::Sender<EventMessage>,
    event_type: EventType,
    source: EventSource,
    severity: EventSeverity,
    payload: serde_json::Value,
    chain_depth: u32,
    trace_id: TraceID,
    agent_id: Option<AgentID>,
    task_id: Option<TaskID>,
) {
    let event_id = EventID::new();
    let timestamp = chrono::Utc::now();

    // Compute HMAC signature over canonical representation
    let canonical = format!(
        "{}|{:?}|{}|{}",
        event_id,
        event_type,
        timestamp.to_rfc3339(),
        chain_depth
    );
    let signature = capability_engine.sign_data(canonical.as_bytes());

    let event = EventMessage {
        id: event_id,
        event_type,
        source,
        payload: payload.clone(),
        severity,
        timestamp,
        signature,
        trace_id,
        chain_depth,
    };

    // Audit log the emission
    if let Err(e) = audit.append(AuditEntry {
        timestamp,
        trace_id,
        event_type: AuditEventType::EventEmitted,
        agent_id,
        task_id,
        tool_id: None,
        details: {
            // Include the original event payload so EventEmitted entries are
            // self-contained and queryable. Guard against the 64 KiB details limit.
            const MAX_PAYLOAD_BYTES: usize = 60 * 1024;
            let payload_value: serde_json::Value = match serde_json::to_string(&payload) {
                Ok(s) if s.len() <= MAX_PAYLOAD_BYTES => payload.clone(),
                Ok(s) => serde_json::json!({
                    "__truncated": true,
                    "original_bytes": s.len(),
                }),
                Err(_) => {
                    serde_json::json!({ "__truncated": true, "error": "serialization_failed" })
                }
            };
            serde_json::json!({
                "event_id": event_id.to_string(),
                "event_type": format!("{:?}", event.event_type),
                "severity": format!("{:?}", severity),
                "chain_depth": chain_depth,
                "payload": payload_value,
            })
        },
        severity: AuditSeverity::Info,
        reversible: false,
        rollback_ref: None,
    }) {
        tracing::error!(error = %e, "Failed to write audit log entry");
    }

    // Count before the send attempt — the event has been emitted regardless of delivery.
    crate::metrics::record_event_emitted();

    // Capture event_type before try_send consumes `event`.
    let event_type_debug = format!("{:?}", event.event_type);

    // Push into the event channel for the EventDispatcher to process.
    if let Err(e) = event_sender.try_send(event) {
        crate::metrics::record_event_dropped();
        tracing::warn!(
            error = %e,
            event_type = %event_type_debug,
            "Event channel full — event dropped (increase kernel.events.channel_capacity under load)"
        );
        // Write directly to the audit log — never re-emit through the event system
        // to avoid infinite recursion if the channel is consistently full.
        if let Err(audit_err) = audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: AuditEventType::EventChannelFull,
            agent_id,
            task_id,
            tool_id: None,
            details: serde_json::json!({
                "dropped_event_type": event_type_debug,
                "error": e.to_string(),
                "hint": "Increase kernel.events.channel_capacity in config if this recurs",
            }),
            severity: AuditSeverity::Warn,
            reversible: false,
            rollback_ref: None,
        }) {
            tracing::error!(
                error = %audit_err,
                "Failed to write EventChannelFull audit entry (double failure: channel full + audit write failed)"
            );
        }
    }
}

impl Kernel {
    /// Emit an event into the event system.
    ///
    /// This builds an `EventMessage`, signs it with the kernel HMAC key,
    /// logs it to the audit trail, and pushes it into the event channel
    /// for asynchronous processing by the `EventDispatcher` task.
    pub async fn emit_event(
        &self,
        event_type: EventType,
        source: EventSource,
        severity: EventSeverity,
        payload: serde_json::Value,
        chain_depth: u32,
    ) {
        self.emit_event_with_trace(
            event_type,
            source,
            severity,
            payload,
            chain_depth,
            None,
            None,
            None,
        )
        .await;
    }

    /// Emit an event and optionally preserve an existing trace ID for
    /// correlation with the surrounding audit trail. Pass `agent_id` and
    /// `task_id` when available so the EventEmitted audit entry is queryable
    /// by agent or task without needing to join through the event payload.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn emit_event_with_trace(
        &self,
        event_type: EventType,
        source: EventSource,
        severity: EventSeverity,
        payload: serde_json::Value,
        chain_depth: u32,
        trace_id: Option<TraceID>,
        agent_id: Option<AgentID>,
        task_id: Option<TaskID>,
    ) {
        emit_signed_event(
            &self.capability_engine,
            &self.audit,
            &self.event_sender,
            event_type,
            source,
            severity,
            payload,
            chain_depth,
            trace_id.unwrap_or_default(),
            agent_id,
            task_id,
        );
    }

    /// Process a communication notification from AgentMessageBus, converting it
    /// into a properly HMAC-signed EventMessage with audit trail.
    pub(crate) async fn process_comm_notification(
        &self,
        notif: crate::agent_message_bus::CommNotification,
    ) {
        self.emit_event(
            notif.event_type,
            EventSource::AgentMessageBus,
            notif.severity,
            notif.payload,
            0,
        )
        .await;
    }

    /// Process a schedule notification from ScheduleManager, converting it
    /// into a properly HMAC-signed EventMessage with audit trail.
    pub(crate) async fn process_schedule_notification(
        &self,
        notif: crate::schedule_manager::ScheduleNotification,
    ) {
        self.emit_event(
            notif.event_type,
            EventSource::Scheduler,
            notif.severity,
            notif.payload,
            0,
        )
        .await;
    }

    /// Process a resource arbiter notification (preemption or deadlock), converting it
    /// into a properly HMAC-signed EventMessage with audit trail.
    pub(crate) async fn process_arbiter_notification(
        &self,
        notif: crate::resource_arbiter::ArbiterNotification,
    ) {
        use crate::resource_arbiter::ArbiterNotification;
        match notif {
            ArbiterNotification::Preemption(p) => {
                self.emit_event(
                    EventType::TaskPreempted,
                    EventSource::TaskScheduler,
                    EventSeverity::Warning,
                    serde_json::json!({
                        "preempted_agent": p.preempted_agent.to_string(),
                        "preempting_agent": p.preempting_agent.to_string(),
                        "resource_id": p.resource_id,
                    }),
                    0,
                )
                .await;
            }
            ArbiterNotification::Deadlock(d) => {
                self.emit_event(
                    EventType::TaskDeadlockDetected,
                    EventSource::TaskScheduler,
                    EventSeverity::Critical,
                    serde_json::json!({
                        "blocked_agent": d.blocked_agent.to_string(),
                        "holder_agent": d.holder_agent.to_string(),
                        "resource_id": d.resource_id,
                    }),
                    0,
                )
                .await;
            }
        }
    }

    /// Process a tool lifecycle notification from ToolRegistry, converting it
    /// into a properly signed EventMessage with audit trail.
    pub(crate) async fn process_tool_lifecycle_event(
        &self,
        event: crate::tool_registry::ToolLifecycleEvent,
    ) {
        use crate::tool_registry::ToolLifecycleEvent;
        match event {
            ToolLifecycleEvent::Installed {
                tool_id,
                tool_name,
                trust_tier,
                description,
            } => {
                self.emit_event(
                    EventType::ToolInstalled,
                    EventSource::ToolRunner,
                    EventSeverity::Info,
                    serde_json::json!({
                        "tool_id": tool_id.to_string(),
                        "tool_name": tool_name,
                        "trust_tier": trust_tier,
                        "description": description,
                    }),
                    0,
                )
                .await;

                // Emit UnverifiedToolInstalled for non-Core tools
                if trust_tier != "Core" {
                    self.emit_event(
                        EventType::UnverifiedToolInstalled,
                        EventSource::ToolRunner,
                        EventSeverity::Warning,
                        serde_json::json!({
                            "tool_id": tool_id.to_string(),
                            "tool_name": tool_name,
                            "trust_tier": trust_tier,
                        }),
                        0,
                    )
                    .await;
                }

                // Emit ToolRegistryUpdated on every install
                self.emit_event(
                    EventType::ToolRegistryUpdated,
                    EventSource::ToolRunner,
                    EventSeverity::Info,
                    serde_json::json!({
                        "action": "installed",
                        "tool_name": tool_name,
                    }),
                    0,
                )
                .await;

                // Refresh the shared tool catalogue so agent-manual reflects the new tool.
                {
                    let registry = self.tool_registry.read().await;
                    let all_tools = registry.list_all();
                    let fresh =
                        agentos_tools::agent_manual::AgentManualTool::summaries_from_registry(
                            &all_tools,
                        );
                    *self.tool_summaries.write().await = fresh;
                    tracing::debug!("agent-manual catalogue refreshed after tool install");
                }
            }
            ToolLifecycleEvent::Removed { tool_id, tool_name } => {
                self.emit_event(
                    EventType::ToolRemoved,
                    EventSource::ToolRunner,
                    EventSeverity::Info,
                    serde_json::json!({
                        "tool_id": tool_id.to_string(),
                        "tool_name": tool_name,
                    }),
                    0,
                )
                .await;

                // Emit ToolRegistryUpdated on every removal
                self.emit_event(
                    EventType::ToolRegistryUpdated,
                    EventSource::ToolRunner,
                    EventSeverity::Info,
                    serde_json::json!({
                        "action": "removed",
                        "tool_name": tool_name,
                    }),
                    0,
                )
                .await;

                // Refresh the shared tool catalogue so agent-manual reflects the removed tool.
                {
                    let registry = self.tool_registry.read().await;
                    let all_tools = registry.list_all();
                    let fresh =
                        agentos_tools::agent_manual::AgentManualTool::summaries_from_registry(
                            &all_tools,
                        );
                    *self.tool_summaries.write().await = fresh;
                    tracing::debug!("agent-manual catalogue refreshed after tool removal");
                }
            }
            ToolLifecycleEvent::ChecksumMismatch {
                tool_name,
                expected,
                actual,
            } => {
                self.emit_event(
                    EventType::ToolChecksumMismatch,
                    EventSource::ToolRunner,
                    EventSeverity::Critical,
                    serde_json::json!({
                        "tool_name": tool_name,
                        "expected_checksum": expected,
                        "actual_checksum": actual,
                    }),
                    0,
                )
                .await;
            }
        }
    }

    /// Process a single event received from the event channel.
    /// Called by the EventDispatcher supervised task.
    pub(crate) async fn process_event(self: &Arc<Self>, event: EventMessage) {
        crate::metrics::record_event_processed();

        // Tee a coarse, lossy view of every event into the realtime broadcast for
        // the control panel's WS/SSE layer. Send errors (no receivers) are ignored.
        let _ = self.realtime_event_sender.send(realtime_event_from(&event));

        // Check chain depth for loop detection
        if event.chain_depth > self.event_bus.max_chain_depth() {
            tracing::warn!(
                event_type = ?event.event_type,
                depth = event.chain_depth,
                "Event loop detected, dropping event"
            );
            self.audit_log(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: event.trace_id,
                event_type: AuditEventType::EventLoopDetected,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "event_id": event.id.to_string(),
                    "event_type": format!("{:?}", event.event_type),
                    "chain_depth": event.chain_depth,
                }),
                severity: AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            });
            return;
        }

        // Evaluate subscriptions
        let matching_subs = self.event_bus.evaluate_subscriptions(&event).await;

        // Exclude the causing agent from being triggered by events about its own
        // activity. A triggered task emits its own TaskStarted/TaskFailed/...,
        // writes its own episodic memory on completion, and its tool calls /
        // messages / delegations emit ToolEvents / AgentCommunication events —
        // so a self-match on any of these is a guaranteed infinite trigger loop
        // (each reaction task re-fires the subscription that spawned it).
        // Applies to AgentAdded (an agent must not be notified of its own
        // addition) and every category whose events are driven by an agent's
        // own task activity. For AgentCommunication only the sender/delegator
        // is excluded — recipients still receive. Events whose payload lacks a
        // recognizable causer key fail open (delivered).
        // `ProcessCrashed` (kernel.rs, process-crash callback) carries the dead
        // process's OWNER — the subscriber that most needs the wake-up — so a
        // category-wide exclusion of SystemHealth would silently break process
        // supervision. The budget pair used to be excluded here by TYPE; it is
        // now handled below by `is_budget_event`, which drops the reaction task
        // for EVERY subscriber, not just the agent that tripped it.
        let self_excludable = is_self_excludable(event.event_type);
        let mut matching_subs = matching_subs;
        if self_excludable {
            if let Some(causer) = causing_agent(&event.payload) {
                let mut kept = Vec::with_capacity(matching_subs.len());
                for sub in matching_subs {
                    if sub.agent_id == causer {
                        tracing::debug!(
                            causer = %causer,
                            event_type = ?event.event_type,
                            subscription_id = %sub.id,
                            "self-excluded subscription (event trigger loop guard)"
                        );
                        // Still record the event passively in the agent's inbox —
                        // write_event is a plain insert (no kernel events), so it
                        // carries no loop risk. Only the reaction *task* is skipped.
                        self.agent_inbox_writer
                            .write_event(
                                sub.agent_id,
                                sub.id.to_string(),
                                event.id.to_string(),
                                &format!("{:?}", event.event_type),
                                event.payload.clone(),
                            )
                            .await;
                    } else {
                        kept.push(sub);
                    }
                }
                matching_subs = kept;
            }
        }

        // Drop subscriptions whose agent is Offline. Offline means no usable LLM
        // adapter — a failed/never-completed connect, an auto-pause by the failure
        // breaker, or a manual disconnect — so a triggered task can only fail.
        // Every other task-spawn path (commands/task.rs, sub_agent.rs, team.rs,
        // webhook_wakeup.rs) already checks this; the event path did not. The event
        // is still recorded in the agent's inbox so it sees what it missed on
        // reconnect (a plain insert — no kernel events, no loop risk).
        let mut skipped_offline = Vec::new();
        {
            let registry = self.agent_registry.read().await;
            matching_subs.retain(|sub| {
                let online = registry
                    .get_by_id(&sub.agent_id)
                    .is_some_and(|a| a.status != AgentStatus::Offline);
                if !online {
                    skipped_offline.push(sub.clone());
                }
                online
            });
        }
        for sub in skipped_offline {
            tracing::debug!(
                agent_id = %sub.agent_id,
                event_type = ?event.event_type,
                subscription_id = %sub.id,
                "skipped subscription — agent is Offline"
            );
            self.agent_inbox_writer
                .write_event(
                    sub.agent_id,
                    sub.id.to_string(),
                    event.id.to_string(),
                    &format!("{:?}", event.event_type),
                    event.payload.clone(),
                )
                .await;
        }

        // Drop subscriptions whose agent is budget-paused. A reaction task for
        // an agent past its pause threshold can only suspend on its first
        // inference — and a suspend-storm is invisible to the failure-streak
        // breaker, which counts only Failed tasks. `check_budget` is read-only,
        // cheap (RwLock read + atomics), and self-healing: it rolls the 24h
        // period internally, so delivery resumes on its own when the budget
        // resets. The event is still recorded in the agent's inbox (a plain
        // insert — no kernel events, no loop risk).
        let mut skipped_broke = Vec::new();
        {
            use crate::cost_tracker::BudgetCheckResult;
            let mut kept = Vec::with_capacity(matching_subs.len());
            for sub in matching_subs {
                match self.cost_tracker.check_budget(&sub.agent_id).await {
                    BudgetCheckResult::PauseRequired { .. }
                    | BudgetCheckResult::HardLimitExceeded { .. } => {
                        skipped_broke.push(sub);
                    }
                    _ => kept.push(sub),
                }
            }
            matching_subs = kept;
        }
        for sub in skipped_broke {
            tracing::debug!(
                agent_id = %sub.agent_id,
                event_type = ?event.event_type,
                subscription_id = %sub.id,
                "skipped subscription — agent is budget-paused"
            );
            self.agent_inbox_writer
                .write_event(
                    sub.agent_id,
                    sub.id.to_string(),
                    event.id.to_string(),
                    &format!("{:?}", event.event_type),
                    event.payload.clone(),
                )
                .await;
        }

        if matching_subs.is_empty() {
            return;
        }

        // Budget events are notifications, never triggers: an agent past its
        // budget can only trip it again on the reaction task's first inference.
        // The event still lands in every matching agent's inbox so the agent
        // sees it on its next turn; the operator is notified separately from
        // the enforcement path in the task executor.
        if is_budget_event(event.event_type) {
            for sub in &matching_subs {
                self.agent_inbox_writer
                    .write_event(
                        sub.agent_id,
                        sub.id.to_string(),
                        event.id.to_string(),
                        &format!("{:?}", event.event_type),
                        event.payload.clone(),
                    )
                    .await;
            }
            tracing::debug!(
                event_type = ?event.event_type,
                matched = matching_subs.len(),
                "budget event recorded to inboxes — no reaction tasks spawned"
            );
            return;
        }

        tracing::debug!(
            event_type = ?event.event_type,
            matched = matching_subs.len(),
            "Event matched subscriptions"
        );

        // Route each subscription: spawn now, or coalesce into the agent's
        // reaction batch so a burst costs one inference instead of N.
        let batching_off = self.config.kernel.events.reaction_batch_window_secs == 0;
        let category = event.event_type.category();
        for sub in &matching_subs {
            if batching_off || bypasses_batching(sub.priority, category) {
                let prompt = self.build_trigger_prompt(&event, sub).await;
                let _ = self
                    .spawn_reaction_task(
                        sub,
                        &event,
                        &prompt,
                        true,
                        serde_json::json!({
                            "event_id": event.id.to_string(),
                            "event_type": format!("{:?}", event.event_type),
                            "subscription_id": sub.id.to_string(),
                        }),
                    )
                    .await;
            } else {
                self.enqueue_reaction(sub, &event).await;
            }
        }
    }

    /// Spawn one triggered task and write its audit trail.
    ///
    /// `trigger_details` is the `EventTriggeredTask` detail payload — single
    /// and batched deliveries record different shapes. `write_inbox` is false
    /// for batched deliveries, which already wrote the event at append time.
    async fn spawn_reaction_task(
        &self,
        sub: &EventSubscription,
        event: &EventMessage,
        prompt: &str,
        write_inbox: bool,
        trigger_details: serde_json::Value,
    ) -> Option<TaskID> {
        match self.create_triggered_task(sub, prompt, event).await {
            Ok(task_id) => {
                if write_inbox {
                    self.agent_inbox_writer
                        .write_event(
                            sub.agent_id,
                            sub.id.to_string(),
                            event.id.to_string(),
                            &format!("{:?}", event.event_type),
                            event.payload.clone(),
                        )
                        .await;
                }

                self.audit_log(AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: event.trace_id,
                    event_type: AuditEventType::EventTriggeredTask,
                    agent_id: Some(sub.agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: trigger_details,
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                self.audit_log(AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: event.trace_id,
                    event_type: AuditEventType::EventDelivered,
                    agent_id: Some(sub.agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "event_id": event.id.to_string(),
                        "subscription_id": sub.id.to_string(),
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                Some(task_id)
            }
            Err(e) => {
                self.audit_log(AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: event.trace_id,
                    event_type: AuditEventType::EventTriggerFailed,
                    agent_id: Some(sub.agent_id),
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "event_id": event.id.to_string(),
                        "event_type": format!("{:?}", event.event_type),
                        "subscription_id": sub.id.to_string(),
                        "failure_reason": e.to_string(),
                        "stage": "create_triggered_task",
                    }),
                    severity: AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
                tracing::warn!(
                    agent_id = %sub.agent_id,
                    error = %e,
                    "Failed to create triggered task for event"
                );
                None
            }
        }
    }

    /// Append a reaction to the agent's pending batch, arming the window timer
    /// on the first item and flushing early once the batch is full.
    ///
    /// The event is written to the inbox here, not at flush time, so the inbox
    /// stays complete even if the batch is later coalesced or dropped.
    async fn enqueue_reaction(self: &Arc<Self>, sub: &EventSubscription, event: &EventMessage) {
        self.agent_inbox_writer
            .write_event(
                sub.agent_id,
                sub.id.to_string(),
                event.id.to_string(),
                &format!("{:?}", event.event_type),
                event.payload.clone(),
            )
            .await;

        let agent_id = sub.agent_id;
        let window = self.config.kernel.events.reaction_batch_window_secs;
        let max_events = self.config.kernel.events.reaction_batch_max_events.max(1);

        let (arm_timer, flush_now) = {
            let mut pending = self.reaction_batcher.pending.lock().await;
            let batch = pending.entry(agent_id).or_default();
            // Hard ceiling on retained items: a batch whose flush is blocked by
            // a long-running reaction task would otherwise grow for as long as
            // the storm lasts. Overflow is inbox-only — it was already written
            // above, so nothing is lost, only the digest is capped.
            if batch.items.len() >= max_events {
                batch.overflow += 1;
                tracing::debug!(
                    %agent_id,
                    max_events,
                    "reaction batch full — event recorded to inbox only"
                );
            } else {
                batch.items.push((sub.clone(), event.clone()));
            }
            let flush_now = batch.items.len() >= max_events;
            let arm_timer = !batch.timer_started && !flush_now;
            if arm_timer {
                batch.timer_started = true;
            }
            (arm_timer, flush_now)
        };

        if flush_now {
            // A timer may still be armed; a double flush is a no-op because the
            // flush takes the whole batch and an empty take returns early.
            self.flush_reactions(agent_id).await;
            return;
        }
        if arm_timer {
            self.arm_batch_timer(agent_id, window);
        }
    }

    /// Spawn the one-shot window timer that flushes an agent's batch.
    fn arm_batch_timer(self: &Arc<Self>, agent_id: AgentID, window: u64) {
        let kernel = Arc::clone(self);
        let token = self.cancellation_token.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = tokio::time::sleep(Duration::from_secs(window)) => {
                    // This timer is consumed. `timer_started` means "a timer
                    // will flush this batch", so clear it before flushing —
                    // a held flush re-arms exactly one if it must wait again.
                    if let Some(batch) = kernel
                        .reaction_batcher
                        .pending
                        .lock()
                        .await
                        .get_mut(&agent_id)
                    {
                        batch.timer_started = false;
                    }
                    kernel.flush_reactions(agent_id).await;
                }
            }
        });
    }

    /// Turn an agent's pending reaction batch into at most one task.
    ///
    /// Holds off while a previous reaction task for the agent is still running
    /// (the terminal task paths call `drain_reactions_after_task`, which flushes
    /// whatever piled up). A single-item batch takes exactly the unbatched path;
    /// several items collapse into one digest-prompted task.
    pub(crate) async fn flush_reactions(self: &Arc<Self>, agent_id: AgentID) {
        // Snapshot the slot and DROP the lock before asking the scheduler about
        // the task — a batcher lock is never held across another subsystem.
        let recorded = self
            .reaction_batcher
            .in_flight
            .lock()
            .await
            .get(&agent_id)
            .copied();
        let free = match recorded {
            None => true,
            Some((slot, started_at)) => {
                // Backstop for a task the scheduler no longer knows about at all
                // (kernel restart, purge) — the liveness check below cannot tell
                // that apart from "not enqueued yet".
                let stale = (chrono::Utc::now() - started_at).num_hours() >= IN_FLIGHT_STALE_HOURS;
                match slot {
                    // A reservation with no id yet is a spawn in progress: busy.
                    None => stale,
                    Some(task_id) => stale || !self.reaction_task_is_live(&task_id).await,
                }
            }
        };

        // Re-take the lock and reserve the slot in one step. The entry must
        // still hold exactly what the liveness check looked at — another flush
        // may have spawned in between — and reserving with `None` closes the
        // window in which two flushes both find an empty map.
        let reserved = {
            let mut in_flight = self.reaction_batcher.in_flight.lock().await;
            let unchanged =
                in_flight.get(&agent_id).map(|(slot, _)| *slot) == recorded.map(|(slot, _)| slot);
            if free && unchanged {
                in_flight.insert(agent_id, (None, chrono::Utc::now()));
                true
            } else {
                false
            }
        };

        // Re-arm rather than relying on the drain hook alone. `drain_reactions_
        // after_task` covers the normal terminal paths, but a reaction task that
        // ends suspended (budget pause), cancelled, or purged never reaches them
        // — and this batch already consumed its one-shot timer, so without a
        // fresh one it would sit unflushed until the process restarted.
        if !reserved {
            tracing::debug!(
                %agent_id,
                "reaction batch held — previous reaction task still running"
            );
            let window = self.config.kernel.events.reaction_batch_window_secs.max(1);
            // Honor `timer_started` here too: once a batch is full, every
            // further event calls this inline, and re-arming unconditionally
            // would stack one timer per event for the life of the storm.
            let rearm = {
                let mut pending = self.reaction_batcher.pending.lock().await;
                match pending.get_mut(&agent_id) {
                    Some(batch) if !batch.items.is_empty() && !batch.timer_started => {
                        batch.timer_started = true;
                        true
                    }
                    _ => false,
                }
            };
            if rearm {
                self.arm_batch_timer(agent_id, window);
            }
            return;
        }

        // ponytail: `pending` self-cleans here (the entry is removed whole) and
        // `in_flight` self-cleans above, so neither map retains a removed or
        // reconnected agent. Residual case: an agent removed between this flush
        // and its task's terminal path keeps one `in_flight` entry until the
        // next flush for that AgentID — which never comes, because a reconnect
        // mints a new one. Bounded by agents-removed-while-reacting, so no
        // removal hook in `cmd_remove_agent`.
        // Take the batch in one statement so the `pending` guard is gone before
        // `in_flight` is touched — the two locks are never nested.
        let taken = self.reaction_batcher.pending.lock().await.remove(&agent_id);
        let (items, overflow) = match taken {
            Some(batch) => (batch.items, batch.overflow),
            None => {
                self.reaction_batcher
                    .in_flight
                    .lock()
                    .await
                    .remove(&agent_id);
                return;
            }
        };
        let Some((_, first_event)) = items.first() else {
            self.reaction_batcher
                .in_flight
                .lock()
                .await
                .remove(&agent_id);
            return;
        };
        // All items share one agent, but not one subscription: an agent with a
        // High sub and a Low sub would otherwise get whichever landed first.
        // The lead sub also names the batch in `trigger_source`.
        let lead_sub = items
            .iter()
            .map(|(sub, _)| sub)
            .max_by_key(|sub| priority_rank(sub.priority))
            .unwrap_or(&items[0].0);

        let spawned = if items.len() == 1 {
            let prompt = self.build_trigger_prompt(first_event, lead_sub).await;
            self.spawn_reaction_task(
                lead_sub,
                first_event,
                &prompt,
                false,
                serde_json::json!({
                    "event_id": first_event.id.to_string(),
                    "event_type": format!("{:?}", first_event.event_type),
                    "subscription_id": lead_sub.id.to_string(),
                }),
            )
            .await
        } else {
            let mut type_counts: BTreeMap<String, usize> = BTreeMap::new();
            for (_, event) in &items {
                *type_counts
                    .entry(format!("{:?}", event.event_type))
                    .or_default() += 1;
            }
            // Without the ids, only the first event of the batch ever gets an
            // `EventDelivered` row — the other N-1 read as emitted-but-dropped.
            let event_ids: Vec<String> = items
                .iter()
                .take(MAX_AUDITED_EVENT_IDS)
                .map(|(_, event)| event.id.to_string())
                .collect();
            let digest = build_batch_digest(&items, overflow, MAX_DIGEST_BYTES);
            let prompt = self.build_batch_prompt(lead_sub, &digest).await;
            self.spawn_reaction_task(
                lead_sub,
                first_event,
                &prompt,
                false,
                serde_json::json!({
                    "batched_count": items.len(),
                    "batched_overflow": overflow,
                    "event_types": type_counts,
                    "event_ids": event_ids,
                    "subscription_id": lead_sub.id.to_string(),
                }),
            )
            .await
        };

        let mut in_flight = self.reaction_batcher.in_flight.lock().await;
        match spawned {
            Some(task_id) => {
                in_flight.insert(agent_id, (Some(task_id), chrono::Utc::now()));
            }
            // A failed spawn must not leave the reservation behind — that would
            // wedge every later flush for this agent until the stale backstop.
            None => {
                in_flight.remove(&agent_id);
            }
        }
    }

    /// Whether the reaction task recorded in `in_flight` is still running.
    ///
    /// `drain_reactions_after_task` is called from only two terminal paths;
    /// timeouts, `task cancel`, denied escalations, budget suspends and
    /// scheduler queue-cap rejections all bypass them and would otherwise wedge
    /// the agent's batch until the stale backstop. Asking the scheduler makes
    /// the gate self-healing instead of patching eight callers.
    async fn reaction_task_is_live(&self, task_id: &TaskID) -> bool {
        match self.scheduler.get_task(task_id).await {
            None => false,
            Some(task) => !matches!(
                task.state,
                TaskState::Complete | TaskState::Failed | TaskState::Cancelled
            ),
        }
    }

    /// Clear a finished reaction task's in-flight slot and flush whatever piled
    /// up behind it. Called from every terminal task path; a no-op for tasks
    /// that were not reaction tasks.
    pub(crate) async fn drain_reactions_after_task(&self, agent_id: AgentID, task_id: TaskID) {
        {
            let mut in_flight = self.reaction_batcher.in_flight.lock().await;
            let is_ours = matches!(in_flight.get(&agent_id), Some((id, _)) if *id == Some(task_id));
            if !is_ours {
                return;
            }
            in_flight.remove(&agent_id);
        }

        let has_pending = self
            .reaction_batcher
            .pending
            .lock()
            .await
            .get(&agent_id)
            .is_some_and(|batch| !batch.items.is_empty());
        if !has_pending {
            return;
        }

        // `flush_reactions` needs an owned `Arc<Self>`; the terminal paths only
        // hold `&self`, so go through the weak self-slot installed at wiring.
        let kernel = {
            let slot = self.self_weak.lock().unwrap_or_else(|e| e.into_inner());
            slot.as_ref().and_then(|weak| weak.upgrade())
        };
        let Some(kernel) = kernel else {
            tracing::debug!(
                %agent_id,
                "reaction drain skipped — kernel self-reference not wired yet"
            );
            return;
        };
        tokio::spawn(async move {
            kernel.flush_reactions(agent_id).await;
        });
    }

    /// Create a task triggered by an event, following the same pattern as
    /// `create_background_task` but with `trigger_source` set.
    async fn create_triggered_task(
        &self,
        sub: &EventSubscription,
        prompt: &str,
        event: &EventMessage,
    ) -> Result<TaskID, AgentOSError> {
        let task_id = TaskID::new();

        // Get the agent's effective permissions
        let registry = self.agent_registry.read().await;
        let agent = registry
            .get_by_id(&sub.agent_id)
            .ok_or_else(|| AgentOSError::AgentNotFound(sub.agent_id.to_string()))?;

        // Keyed off the AGENT, not the lead subscription. A batched reaction
        // task may span several of this agent's subscriptions (the lead is
        // picked by priority), so keying this off `sub` would hand the whole
        // batch the lead subscription's scope. Same agent either way today —
        // this is a latent hazard, not a live one.
        let effective_permissions = registry.compute_effective_permissions(&sub.agent_id);
        let agent_id = agent.id;
        drop(registry);

        // Issue a capability token for this triggered task
        let capability_token = self.capability_engine.issue_token(
            task_id,
            agent_id,
            BTreeSet::new(), // All tools available based on permissions
            BTreeSet::from([
                IntentTypeFlag::Read,
                IntentTypeFlag::Write,
                IntentTypeFlag::Execute,
                IntentTypeFlag::Query,
                IntentTypeFlag::Observe,
                IntentTypeFlag::Message,
                IntentTypeFlag::Delegate,
                IntentTypeFlag::Broadcast,
                IntentTypeFlag::Escalate,
                IntentTypeFlag::Subscribe,
                IntentTypeFlag::Unsubscribe,
            ]),
            effective_permissions,
            Duration::from_secs(self.config.kernel.default_task_timeout_secs),
        )?;

        // Map subscription priority to task priority
        let priority = priority_rank(sub.priority);

        let task = AgentTask {
            id: task_id,
            state: TaskState::Queued,
            agent_id,
            capability_token,
            assigned_llm: Some(agent_id),
            priority,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(self.config.kernel.default_task_timeout_secs),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            max_iterations: None,
            trigger_source: Some(TriggerSource {
                event_id: event.id,
                event_type: event.event_type,
                subscription_id: sub.id,
                chain_depth: event.chain_depth,
            }),
            autonomous: false,
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            spawner_agent_id: None,
            tool_categories: None,
            disable_tool_scoping: false,
            // `trigger_source` above already carries the event's depth;
            // `event_chain_depth()` derives it. Do not double-count here.
            chain_depth: 0,
        };

        self.scheduler.enqueue(task).await;

        Ok(task_id)
    }
}

#[cfg(test)]
mod trigger_loop_guard_tests {
    use super::*;

    /// 2026-08-31: agent OSS subscribed to the SystemHealth *category*, so its
    /// own BudgetWarning/BudgetExhausted woke it, and each woken task tripped
    /// the budget again on its first inference — 69 self-fed tasks in one hour,
    /// ending with the agent hard-suspended and every chat turn failing.
    /// The pair now takes the inbox-only path in `process_event` for EVERY
    /// subscriber, which subsumes the self-exclusion it used to rely on.
    #[test]
    fn budget_events_take_the_inbox_only_path() {
        assert!(is_budget_event(EventType::BudgetWarning));
        assert!(is_budget_event(EventType::BudgetExhausted));
        assert!(!is_budget_event(EventType::ProcessCrashed));
        // Self-exclusion is no longer what protects them.
        assert!(!is_self_excludable(EventType::BudgetWarning));
        assert!(!is_self_excludable(EventType::BudgetExhausted));
    }

    /// The rest of SystemHealth must stay deliverable: `ProcessCrashed` carries
    /// the dead process's OWNER, the subscriber that most needs the wake-up.
    #[test]
    fn other_system_health_events_are_not_self_excludable() {
        assert!(!is_self_excludable(EventType::ProcessCrashed));
        assert!(!is_self_excludable(EventType::MemoryPressure));
        assert!(!is_self_excludable(EventType::DiskSpaceLow));
    }

    #[test]
    fn agent_driven_categories_are_self_excludable() {
        assert!(is_self_excludable(EventType::AgentAdded));
        assert!(is_self_excludable(EventType::TaskFailed));
    }

    #[test]
    fn causing_agent_reads_the_known_keys() {
        let id = AgentID::new();
        for key in ["agent_id", "from_agent", "delegating_agent_id"] {
            let payload = serde_json::json!({ key: id.to_string() });
            assert_eq!(causing_agent(&payload), Some(id), "key {key}");
        }
        // A payload naming the agent only by display name fails open.
        assert_eq!(causing_agent(&serde_json::json!({ "agent": "OSS" })), None);
    }
}

#[cfg(test)]
mod reaction_batching_tests {
    use super::*;
    use serde_json::json;

    fn sub(priority: SubscriptionPriority) -> EventSubscription {
        EventSubscription {
            id: SubscriptionID::new(),
            agent_id: AgentID::new(),
            event_type_filter: EventTypeFilter::All,
            filter: None,
            priority,
            throttle: ThrottlePolicy::default(),
            enabled: true,
            created_at: chrono::Utc::now(),
        }
    }

    fn event(event_type: EventType, payload: serde_json::Value) -> EventMessage {
        EventMessage {
            id: EventID::new(),
            event_type,
            source: EventSource::HardwareAbstractionLayer,
            payload,
            severity: EventSeverity::Info,
            timestamp: chrono::Utc::now(),
            signature: Vec::new(),
            trace_id: TraceID::new(),
            chain_depth: 0,
        }
    }

    fn push_ev(
        items: &mut Vec<(EventSubscription, EventMessage)>,
        event_type: EventType,
        payload: serde_json::Value,
    ) {
        items.push((
            sub(SubscriptionPriority::Normal),
            event(event_type, payload),
        ));
    }

    #[test]
    fn critical_and_agent_communication_bypass_the_window() {
        let hw = EventCategory::HardwareEvents;
        assert!(bypasses_batching(SubscriptionPriority::Critical, hw));
        let comms = EventCategory::AgentCommunication;
        assert!(bypasses_batching(SubscriptionPriority::Normal, comms));
        assert!(!bypasses_batching(SubscriptionPriority::Normal, hw));
        let health = EventCategory::SystemHealth;
        assert!(!bypasses_batching(SubscriptionPriority::High, health));
    }

    /// The storm case: 60 same-shape events must collapse to a handful of lines.
    #[test]
    fn identical_shape_events_collapse() {
        let mut items = Vec::new();
        for i in 0..60 {
            let payload = json!({ "device": "gpu0", "seq": i });
            push_ev(&mut items, EventType::HardwareAccessGranted, payload);
        }
        let digest = build_batch_digest(&items, 0, 4096);
        assert!(digest.len() < 1024, "digest too large: {}", digest.len());
        assert!(digest.starts_with("[EVENT BATCH] 60 events coalesced"));
        assert!(digest.contains("HardwareAccessGranted x60"));
        // Constant key collapses; varying key is sampled.
        assert!(digest.contains("  device=gpu0"));
        assert!(digest.contains("  seq: 0, 1, 2"));
        assert!(digest.lines().count() <= 6);
    }

    #[test]
    fn varying_key_lists_ten_samples_then_counts_the_rest() {
        let mut items = Vec::new();
        for i in 0..25 {
            let payload = json!({ "host": format!("h{i}") });
            push_ev(&mut items, EventType::MemoryPressure, payload);
        }
        let digest = build_batch_digest(&items, 0, 4096);
        let expected = "h0, h1, h2, h3, h4, h5, h6, h7, h8, h9 (+15 more)";
        assert!(digest.contains(expected), "digest was: {digest}");
        assert!(!digest.contains("h10,"));
    }

    #[test]
    fn nested_values_are_skipped() {
        let mut items = Vec::new();
        for x in 1..3 {
            let payload = json!({ "host": "a", "detail": { "x": x }, "l": [x] });
            push_ev(&mut items, EventType::MemoryPressure, payload);
        }
        let digest = build_batch_digest(&items, 0, 4096);
        assert!(digest.contains("  host=a"));
        assert!(!digest.contains("detail"));
        assert!(!digest.contains("  l:"));
    }

    #[test]
    fn over_budget_input_degrades_through_the_ladder() {
        let mut items = Vec::new();
        for i in 0..50 {
            let payload = json!({
                "host": format!("host-number-{i}"),
                "region": format!("region-{i}"),
                "note": "x".repeat(40),
            });
            push_ev(&mut items, EventType::MemoryPressure, payload);
        }
        for max in [4096usize, 400, 200, 60, 20] {
            let digest = build_batch_digest(&items, 0, max);
            let len = digest.len();
            assert!(len <= max, "max {max} exceeded: {len} bytes");
        }
        // A generous budget keeps the 10-sample rung; a tight one drops samples.
        assert!(build_batch_digest(&items, 0, 4096).contains("(+40 more)"));
        assert!(!build_batch_digest(&items, 0, 200).contains("(+40 more)"));
    }

    #[test]
    fn single_event_digest_is_sane() {
        let mut items = Vec::new();
        let payload = json!({ "pid": 42, "ok": false });
        push_ev(&mut items, EventType::ProcessCrashed, payload);
        let digest = build_batch_digest(&items, 0, 4096);
        assert!(digest.contains("[EVENT BATCH] 1 events coalesced"));
        assert!(digest.contains("ProcessCrashed x1"));
        assert!(digest.contains("  pid=42"));
        assert!(digest.contains("  ok=false"));
    }

    #[test]
    fn digest_is_deterministic_and_first_seen_ordered() {
        let mut items = Vec::new();
        push_ev(&mut items, EventType::MemoryPressure, json!({ "b": 2 }));
        push_ev(&mut items, EventType::ProcessCrashed, json!({ "pid": 7 }));
        push_ev(&mut items, EventType::MemoryPressure, json!({ "b": 3 }));
        let first = build_batch_digest(&items, 0, 4096);
        assert_eq!(first, build_batch_digest(&items, 0, 4096));
        assert!(first.contains("MemoryPressure x2"));
        assert!(first.contains("ProcessCrashed x1"));
        let mp = first.find("MemoryPressure").unwrap_or(usize::MAX);
        let pc = first.find("ProcessCrashed").unwrap_or(usize::MAX);
        assert!(mp < pc, "type order not first-seen: {first}");
    }

    /// Events dropped by the `reaction_batch_max_events` cap are inbox-only;
    /// the header must say so instead of claiming the batch was the whole storm.
    #[test]
    fn overflow_is_named_in_the_header() {
        let mut items = Vec::new();
        for i in 0..3 {
            push_ev(&mut items, EventType::MemoryPressure, json!({ "seq": i }));
        }
        let digest = build_batch_digest(&items, 4997, 4096);
        assert!(
            digest.starts_with(
                "[EVENT BATCH] 3 events coalesced within one window (+4997 more in your inbox)."
            ),
            "digest was: {digest}"
        );
        // No overflow, no note.
        assert!(!build_batch_digest(&items, 0, 4096).contains("more in your inbox"));
    }

    /// The last-resort clamp cuts on a byte ladder; a cut inside a multibyte
    /// value must not panic or overshoot the budget.
    #[test]
    fn byte_clamp_never_splits_a_code_point() {
        // The clamp itself: every budget lands mid-character somewhere here.
        let multi = "ページ・déjà・🌍".repeat(8);
        for max in 0..40usize {
            let out = clamp_bytes(multi.clone(), max);
            assert!(out.len() <= max, "max {max} exceeded: {}", out.len());
            assert!(multi.starts_with(&out), "clamp must only truncate");
        }
        // And through the digest, whose payload values are multibyte.
        let mut items = Vec::new();
        for i in 0..30 {
            let payload = json!({ "hôte": format!("マシン-{i}-🌍"), "état": "dégradé" });
            push_ev(&mut items, EventType::MemoryPressure, payload);
        }
        for max in [4096usize, 500, 120, 37, 9, 1, 0] {
            let digest = build_batch_digest(&items, 7, max);
            assert!(digest.len() <= max, "max {max} exceeded: {}", digest.len());
        }
    }

    #[test]
    fn long_values_are_clipped_before_the_budget_ladder() {
        let mut items = Vec::new();
        for c in ["y", "z"] {
            let payload = json!({ "blob": c.repeat(500) });
            push_ev(&mut items, EventType::MemoryPressure, payload);
        }
        let digest = build_batch_digest(&items, 0, 4096);
        assert!(digest.contains('…'));
        assert!(digest.len() < 300, "digest was: {digest}");
    }
}
