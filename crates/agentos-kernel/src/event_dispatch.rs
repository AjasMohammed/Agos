use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_capability::CapabilityEngine;
use agentos_types::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use crate::kernel::Kernel;

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
        details: serde_json::json!({
            "event_id": event_id.to_string(),
            "event_type": format!("{:?}", event.event_type),
            "severity": format!("{:?}", severity),
            "chain_depth": chain_depth,
        }),
        severity: AuditSeverity::Info,
        reversible: false,
        rollback_ref: None,
    }) {
        tracing::error!(error = %e, "Failed to write audit log entry");
    }

    // Push into the event channel for the EventDispatcher to process
    if let Err(e) = event_sender.try_send(event) {
        tracing::error!(error = %e, "Failed to send event to dispatcher channel (possibly full or closed)");
    }
}

impl Kernel {
    /// Emit an event into the event system.
    ///
    /// This builds an `EventMessage`, signs it with the kernel HMAC key,
    /// logs it to the audit trail, and pushes it into the event channel
    /// for asynchronous processing by the `EventDispatcher` task.
    pub(crate) async fn emit_event(
        &self,
        event_type: EventType,
        source: EventSource,
        severity: EventSeverity,
        payload: serde_json::Value,
        chain_depth: u32,
    ) {
        self.emit_event_with_trace(event_type, source, severity, payload, chain_depth, None)
            .await;
    }

    /// Emit an event and optionally preserve an existing trace ID for
    /// correlation with the surrounding audit trail.
    pub(crate) async fn emit_event_with_trace(
        &self,
        event_type: EventType,
        source: EventSource,
        severity: EventSeverity,
        payload: serde_json::Value,
        chain_depth: u32,
        trace_id: Option<TraceID>,
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
            None,
            None,
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
            }
        }
    }

    /// Process a single event received from the event channel.
    /// Called by the EventDispatcher supervised task.
    pub(crate) async fn process_event(self: &Arc<Self>, event: EventMessage) {
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

        if matching_subs.is_empty() {
            return;
        }

        tracing::debug!(
            event_type = ?event.event_type,
            matched = matching_subs.len(),
            "Event matched subscriptions"
        );

        // For each matching subscription, create a triggered task
        for sub in &matching_subs {
            let prompt = self.build_trigger_prompt(&event, sub).await;
            match self.create_triggered_task(sub, &prompt, &event).await {
                Ok(task_id) => {
                    self.audit_log(AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: event.trace_id,
                        event_type: AuditEventType::EventTriggeredTask,
                        agent_id: Some(sub.agent_id),
                        task_id: Some(task_id),
                        tool_id: None,
                        details: serde_json::json!({
                            "event_id": event.id.to_string(),
                            "event_type": format!("{:?}", event.event_type),
                            "subscription_id": sub.id.to_string(),
                        }),
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
                }
            }
        }
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
                IntentTypeFlag::Escalate,
                IntentTypeFlag::Subscribe,
                IntentTypeFlag::Unsubscribe,
            ]),
            effective_permissions,
            Duration::from_secs(self.config.kernel.default_task_timeout_secs),
        )?;

        // Map subscription priority to task priority
        let priority = match sub.priority {
            SubscriptionPriority::Critical => 1,
            SubscriptionPriority::High => 3,
            SubscriptionPriority::Normal => 5,
            SubscriptionPriority::Low => 8,
        };

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
            trigger_source: Some(TriggerSource {
                event_id: event.id,
                event_type: event.event_type,
                subscription_id: sub.id,
                chain_depth: event.chain_depth,
            }),
        };

        self.scheduler.enqueue(task).await;

        Ok(task_id)
    }
}
