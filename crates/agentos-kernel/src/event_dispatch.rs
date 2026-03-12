use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_types::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use crate::kernel::Kernel;

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
        let event_id = EventID::new();
        let trace_id = TraceID::new();
        let timestamp = chrono::Utc::now();

        // Compute HMAC signature over canonical representation
        let canonical = format!(
            "{}|{:?}|{}|{}",
            event_id,
            event_type,
            timestamp.to_rfc3339(),
            chain_depth
        );
        let signature = self.capability_engine.sign_data(canonical.as_bytes());

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
        let _ = self.audit.append(AuditEntry {
            timestamp,
            trace_id,
            event_type: AuditEventType::EventEmitted,
            agent_id: None,
            task_id: None,
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
        });

        // Push into the event channel for the EventDispatcher to process
        if let Err(e) = self.event_sender.send(event) {
            tracing::error!(error = %e, "Failed to send event to dispatcher channel");
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
            let _ = self.audit.append(AuditEntry {
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
                    let _ = self.audit.append(AuditEntry {
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

                    let _ = self.audit.append(AuditEntry {
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
