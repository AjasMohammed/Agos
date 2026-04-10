use super::Hook;
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_types::{HookEvent, HookResult, TraceID};
use async_trait::async_trait;
use std::sync::Arc;

/// Built-in hook that writes an `AuditLog` entry for every lifecycle event.
///
/// Registered as the *first* hook during kernel boot so that every event
/// produces an audit record regardless of what other hooks do (including
/// abort hooks that fire after this one).
pub struct AuditHook {
    audit: Arc<AuditLog>,
}

impl AuditHook {
    pub fn new(audit: Arc<AuditLog>) -> Arc<Self> {
        Arc::new(Self { audit })
    }
}

#[async_trait]
impl Hook for AuditHook {
    fn name(&self) -> &'static str {
        "audit"
    }

    fn handles(&self, _event: &HookEvent) -> bool {
        true // audit hook observes every event
    }

    async fn on_event(&self, event: &HookEvent) -> HookResult {
        let (event_type, details) = match event {
            HookEvent::TaskStart { task_id, agent_id } => (
                AuditEventType::TaskStateChanged,
                serde_json::json!({ "task_id": task_id, "agent_id": agent_id, "state": "Running" }),
            ),
            HookEvent::TaskEnd {
                task_id,
                agent_id,
                success,
            } => (
                if *success {
                    AuditEventType::TaskCompleted
                } else {
                    AuditEventType::TaskFailed
                },
                serde_json::json!({
                    "task_id": task_id,
                    "agent_id": agent_id,
                    "success": success
                }),
            ),
            HookEvent::ToolPre {
                task_id, tool_name, ..
            } => (
                AuditEventType::ToolExecutionStarted,
                serde_json::json!({ "task_id": task_id, "tool_name": tool_name }),
            ),
            HookEvent::ToolPost {
                task_id,
                tool_name,
                duration_ms,
                ..
            } => (
                AuditEventType::ToolExecutionCompleted,
                serde_json::json!({
                    "task_id": task_id,
                    "tool_name": tool_name,
                    "duration_ms": duration_ms
                }),
            ),
            HookEvent::AgentSpawned {
                parent_task,
                child_agent,
            } => (
                AuditEventType::AgentConnected,
                serde_json::json!({
                    "parent_task": parent_task,
                    "child_agent": child_agent,
                    "kind": "spawned"
                }),
            ),
            HookEvent::CheckpointWritten { task_id } => (
                AuditEventType::CheckpointWritten,
                serde_json::json!({ "task_id": task_id }),
            ),
            HookEvent::ConfigReloaded => (
                AuditEventType::KernelConfigChanged,
                serde_json::json!({ "source": "hook" }),
            ),
            // HookEvent::Shutdown is intentionally skipped here.
            // The run_loop already calls audit_shutdown() which writes the authoritative
            // KernelShutdown entry. Writing it again from the hook would create duplicates.
            HookEvent::Shutdown => return HookResult::Continue,
            HookEvent::ChannelMessageReceived { channel_id, sender } => (
                AuditEventType::InboundMessageReceived,
                serde_json::json!({ "channel_id": channel_id, "sender": sender }),
            ),
            // No dedicated outbound message audit type yet; use the closest available.
            HookEvent::ChannelMessageSent {
                channel_id,
                recipient,
            } => (
                AuditEventType::IntentCompleted,
                serde_json::json!({ "channel_id": channel_id, "recipient": recipient, "kind": "outbound_message" }),
            ),
            _ => return HookResult::Continue, // unknown/future events: skip silently
        };

        // Extract structured IDs from the event so the audit log is queryable.
        // ToolPre/ToolPost no longer carry ToolID (we use tool_name instead),
        // so entry_tool_id is always None for those events.
        let (entry_agent_id, entry_task_id) = match event {
            HookEvent::TaskStart { task_id, agent_id }
            | HookEvent::TaskEnd {
                task_id, agent_id, ..
            } => (Some(*agent_id), Some(*task_id)),
            HookEvent::ToolPre {
                task_id, agent_id, ..
            }
            | HookEvent::ToolPost {
                task_id, agent_id, ..
            } => (Some(*agent_id), Some(*task_id)),
            HookEvent::AgentSpawned {
                parent_task,
                child_agent,
            } => (Some(*child_agent), Some(*parent_task)),
            HookEvent::CheckpointWritten { task_id } => (None, Some(*task_id)),
            _ => (None, None),
        };

        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type,
            agent_id: entry_agent_id,
            task_id: entry_task_id,
            tool_id: None, // tool_name is in details JSON; ToolID not available in hook context
            details,
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        };

        // Best-effort append — hook failures must never crash the kernel.
        if let Err(e) = self.audit.append(entry) {
            tracing::warn!(hook = "audit", error = %e, "Failed to write audit entry from hook");
        }

        HookResult::Continue
    }
}
