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
        // One trace_id per hook invocation. The generic ToolExecutionCompleted
        // entry AND the typed `HostPackageInstalled` follow-up share this id
        // so an operator querying by trace_id sees both rows for one tool
        // call (review finding I5). For non-tool events the id is still
        // unique per call which is fine — there is no second entry to join.
        let hook_trace_id = TraceID::new();

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
            trace_id: hook_trace_id,
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

        // Tool-specific structured audit: when a privileged tool such as
        // `host-package-install` completes, emit a dedicated typed event in
        // addition to the generic ToolExecutionCompleted entry above. The
        // tool's JSON output already carries every field operators care
        // about (manager, package, version, exit_code, duration_ms,
        // escalator, manager_path); we just unpack it into the typed event.
        if let HookEvent::ToolPost {
            task_id,
            agent_id,
            tool_name,
            output_json,
            duration_ms,
        } = event
        {
            if tool_name == "host-package-install" {
                self.emit_host_package_audit(
                    hook_trace_id,
                    *task_id,
                    *agent_id,
                    output_json,
                    *duration_ms,
                )
                .await;
            }
        }

        HookResult::Continue
    }
}

impl AuditHook {
    /// Parse the JSON returned by `host-package-install` and emit a typed
    /// audit event (`HostPackageInstalled` on success, `HostPackageInstallDenied`
    /// on non-zero exit, `HostPackageInstallTimeout` when the tool reported
    /// a timeout). Best-effort — JSON parse failures are logged and ignored
    /// so the generic `ToolExecutionCompleted` entry still stands.
    async fn emit_host_package_audit(
        &self,
        trace_id: TraceID,
        task_id: agentos_types::TaskID,
        agent_id: agentos_types::AgentID,
        output_json: &str,
        duration_ms: u64,
    ) {
        const STDERR_AUDIT_CAP: usize = 4096;

        let parsed: serde_json::Value = match serde_json::from_str(output_json) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "host-package-install: output was not parseable JSON; \
                     skipping typed audit event (generic entry stands)"
                );
                return;
            }
        };

        // Detect the timeout shape (`run_with_timeout` returns
        // ToolExecutionFailed { reason } which surfaces as `error` in
        // the post-hook output JSON, not as a `HostPackageInstallResult`).
        let is_timeout = parsed
            .get("error")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("timed out"));

        let event_type = if is_timeout {
            AuditEventType::HostPackageInstallTimeout
        } else {
            match parsed.get("installed").and_then(|v| v.as_bool()) {
                Some(true) => AuditEventType::HostPackageInstalled,
                Some(false) => AuditEventType::HostPackageInstallDenied,
                // Unknown shape — likely an early validation error before
                // the tool ran. Skip the typed event.
                None => return,
            }
        };

        let severity = match event_type {
            AuditEventType::HostPackageInstalled => AuditSeverity::Info,
            _ => AuditSeverity::Warn,
        };

        // Bound stderr to a fixed cap. apt-get/dnf can emit kilobytes of
        // dependency-tree output on failure; storing all of it inflates the
        // audit DB without giving operators useful signal beyond the first
        // few KB (review finding I4).
        let stderr_capped = parsed.get("stderr").and_then(|v| v.as_str()).map(|s| {
            if s.len() > STDERR_AUDIT_CAP {
                format!(
                    "{}…[truncated {} bytes]",
                    &s[..STDERR_AUDIT_CAP],
                    s.len() - STDERR_AUDIT_CAP
                )
            } else {
                s.to_string()
            }
        });

        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            // Reuse the trace_id from the surrounding ToolPost hook so the
            // generic and typed entries can be joined by trace.
            trace_id,
            event_type,
            agent_id: Some(agent_id),
            task_id: Some(task_id),
            tool_id: None,
            details: serde_json::json!({
                "tool_name": "host-package-install",
                "duration_ms": duration_ms,
                "package": parsed.get("package"),
                "version": parsed.get("version"),
                "manager": parsed.get("manager"),
                "manager_path": parsed.get("manager_path"),
                "exit_code": parsed.get("exit_code"),
                "escalator": parsed.get("escalator"),
                "denial_reason": parsed.get("denial_reason"),
                "stderr": stderr_capped,
            }),
            severity,
            reversible: false,
            rollback_ref: None,
        };

        if let Err(e) = self.audit.append(entry) {
            tracing::warn!(
                hook = "audit",
                error = %e,
                "Failed to write host-package-install audit entry"
            );
        }
    }
}
