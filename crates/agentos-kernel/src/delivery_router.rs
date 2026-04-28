use crate::kernel::Kernel;
use agentos_types::delivery::{DeliveryMode, NotifyTarget};
use agentos_types::schedule::{RunParentKind, RunState, ScheduledRun};
use agentos_types::*;
use chrono::Utc;
use std::collections::HashMap;

impl Kernel {
    /// Route a completed `ScheduledRun` to its delivery target.
    ///
    /// Called from the task completion path after the run row has been updated
    /// to `Complete` or `Failed`. Non-fatal: logs warnings on error rather than
    /// propagating so a delivery failure cannot crash the completion path.
    pub(crate) async fn dispatch_scheduled_delivery(&self, run_id: RunID) {
        let store = match self.schedule_manager.store() {
            Some(s) => s.clone(),
            None => return,
        };

        let run = match store.get_run(run_id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::warn!(run_id = %run_id, "DeliveryRouter: run not found in store");
                return;
            }
            Err(e) => {
                tracing::warn!(run_id = %run_id, error = %e, "DeliveryRouter: failed to load run");
                return;
            }
        };

        // Idempotency guard: skip if already delivered.
        if run.delivered {
            tracing::debug!(run_id = %run_id, "DeliveryRouter: already delivered, skipping");
            return;
        }

        // Silent delivery requires no network/inbox action. Mark as delivered so
        // future sweepers don't treat undelivered-Silent runs as pending work.
        if matches!(run.delivery, DeliveryMode::Silent) {
            let mut silent_run = run;
            silent_run.delivered = true;
            silent_run.delivered_at = Some(Utc::now());
            if let Err(e) = store.upsert_run(silent_run).await {
                tracing::warn!(run_id = %run_id, error = %e, "DeliveryRouter: failed to mark Silent run as delivered");
            }
            return;
        }

        let parent_name = self.resolve_run_parent_name(&run).await;

        let delivery_result = match run.delivery.clone() {
            DeliveryMode::Silent => Ok(()),
            DeliveryMode::Direct {
                target,
                subject,
                priority,
            } => {
                self.deliver_direct_scheduled(&run, &parent_name, target, subject, priority)
                    .await
            }
            DeliveryMode::ViaAgent {
                agent_id,
                max_depth,
            } => {
                self.deliver_via_agent_scheduled(&run, &parent_name, agent_id, max_depth)
                    .await
            }
        };

        let mut updated = run;
        match delivery_result {
            Ok(()) => {
                updated.delivered = true;
                updated.delivered_at = Some(Utc::now());
                self.emit_event(
                    EventType::ScheduledTaskDelivered,
                    EventSource::TaskScheduler,
                    EventSeverity::Info,
                    serde_json::json!({
                        "run_id": updated.run_id.to_string(),
                        "parent_id": updated.parent_id.to_string(),
                        "parent_kind": updated.parent_kind.as_str(),
                    }),
                    0,
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %updated.run_id,
                    error = %e,
                    "DeliveryRouter: delivery failed"
                );
                updated.delivery_error = Some(e.to_string());
                self.emit_event(
                    EventType::ScheduledTaskDeliveryFailed,
                    EventSource::TaskScheduler,
                    EventSeverity::Warning,
                    serde_json::json!({
                        "run_id": updated.run_id.to_string(),
                        "error": e.to_string(),
                    }),
                    0,
                )
                .await;
            }
        }

        if let Err(e) = store.upsert_run(updated).await {
            tracing::warn!(run_id = %run_id, error = %e, "DeliveryRouter: failed to persist updated run");
        }
    }

    async fn resolve_run_parent_name(&self, run: &ScheduledRun) -> String {
        // Use the name captured at fire time when available — this is the only
        // reliable source for Timers (which are evicted from memory on fire).
        if let Some(name) = &run.parent_name {
            return name.clone();
        }
        match run.parent_kind {
            RunParentKind::Schedule => self
                .schedule_manager
                .get_job(&run.parent_id)
                .await
                .map(|j| j.name)
                .unwrap_or_else(|| run.parent_id.to_string()),
            RunParentKind::OnceJob => self
                .schedule_manager
                .list_once_jobs()
                .await
                .into_iter()
                .find(|j| j.id == run.parent_id)
                .map(|j| j.name)
                .unwrap_or_else(|| run.parent_id.to_string()),
            RunParentKind::Timer => run.parent_id.to_string(),
        }
    }

    async fn deliver_direct_scheduled(
        &self,
        run: &ScheduledRun,
        parent_name: &str,
        target: NotifyTarget,
        subject: Option<String>,
        priority: String,
    ) -> Result<(), AgentOSError> {
        let subject = subject.unwrap_or_else(|| format!("{parent_name}: scheduled task complete"));
        let body = render_run_body(run, parent_name);

        match target {
            NotifyTarget::UserInbox => {
                let prio = parse_priority_str(&priority);
                let msg = UserMessage {
                    id: NotificationID::new(),
                    from: NotificationSource::Kernel,
                    task_id: run.task_id,
                    trace_id: TraceID::new(),
                    kind: UserMessageKind::Notification,
                    priority: prio,
                    subject,
                    body,
                    interaction: None,
                    delivery_status: HashMap::new(),
                    response: None,
                    created_at: Utc::now(),
                    expires_at: None,
                    read: false,
                    thread_id: Some(run.run_id.to_string()),
                    reply_to_external_id: None,
                };
                self.notification_router.deliver(msg).await.map_err(|e| {
                    AgentOSError::KernelError {
                        reason: e.to_string(),
                    }
                })?;
            }
            NotifyTarget::Channel { id } => {
                use agentos_channels::types::{MessageContent, OutboundMessage};
                let text = format!("**{subject}**\n\n{body}");
                let outbound = OutboundMessage {
                    channel_instance_id: id.to_string(),
                    content: MessageContent::Text(text),
                    thread_id: None,
                };
                self.channel_manager.send(&id.to_string(), outbound).await?;
            }
            NotifyTarget::File { path } => {
                use std::path::{Component, Path};
                // Reject absolute paths and any non-Normal components (., .., root, prefix).
                // Only simple relative names like "output/result.txt" are accepted.
                let p = Path::new(&path);
                let unsafe_component = p.components().any(|c| !matches!(c, Component::Normal(_)));
                if unsafe_component || path.as_bytes().contains(&0u8) {
                    return Err(AgentOSError::PermissionDenied {
                        resource: path,
                        operation: "write".into(),
                    });
                }
                // Confine to a dedicated subdirectory under the kernel data dir.
                let base = std::path::PathBuf::from(&self.config.tools.data_dir)
                    .join("scheduled-delivery");
                let full = base.join(p);
                if let Some(parent) = full.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        AgentOSError::KernelError {
                            reason: format!("File delivery mkdir failed: {e}"),
                        }
                    })?;
                }
                tokio::fs::write(&full, body.as_bytes())
                    .await
                    .map_err(|e| AgentOSError::KernelError {
                        reason: format!("File delivery write failed: {e}"),
                    })?;
            }
        }
        Ok(())
    }

    async fn deliver_via_agent_scheduled(
        &self,
        run: &ScheduledRun,
        parent_name: &str,
        agent_id_override: Option<AgentID>,
        max_depth: u8,
    ) -> Result<(), AgentOSError> {
        const HARD_CAP: u8 = 3;
        let depth = run.delivery_depth.unwrap_or(0);
        let cap = max_depth.min(HARD_CAP);
        if max_depth > HARD_CAP {
            tracing::warn!(
                run_id = %run.run_id,
                requested = max_depth,
                effective = cap,
                "ViaAgent max_depth clamped to hard ceiling of {HARD_CAP}"
            );
        }
        if depth > cap {
            return Err(AgentOSError::KernelError {
                reason: format!("delivery depth {depth} exceeds cap {cap}"),
            });
        }

        let target_agent_id = agent_id_override.or(run.creator_agent_id).ok_or_else(|| {
            AgentOSError::KernelError {
                reason: "ViaAgent delivery: no target agent (no override and creator is system)"
                    .into(),
            }
        })?;

        let result_preview = run
            .result
            .as_ref()
            .and_then(|v| v.get("result"))
            .and_then(|v| v.as_str())
            .unwrap_or("(no result)")
            .chars()
            .take(200)
            .collect::<String>();

        let Some(task_id) = run.task_id else {
            return Err(AgentOSError::KernelError {
                reason: "ViaAgent delivery: run has no task_id (task never launched)".into(),
            });
        };

        self.agent_inbox_writer
            .write_scheduled(
                target_agent_id,
                task_id,
                parent_name,
                matches!(run.state, RunState::Complete),
                serde_json::json!({
                    "run_id": run.run_id.to_string(),
                    "state": run.state.as_str(),
                    "result_preview": result_preview,
                    "error": run.error,
                    "delivery_depth": depth + 1,
                }),
            )
            .await;

        Ok(())
    }
}

fn render_run_body(run: &ScheduledRun, parent_name: &str) -> String {
    let state_str = run.state.as_str();
    let result_str = run
        .result
        .as_ref()
        .and_then(|v| v.get("result"))
        .and_then(|v| v.as_str())
        .unwrap_or("(no result)")
        .chars()
        .take(500)
        .collect::<String>();
    match run.error.as_deref() {
        Some(err) if !err.is_empty() => format!(
            "**Schedule:** {parent_name}\n**Status:** {state_str}\n**Error:** {err}\n**Result:** {result_str}"
        ),
        _ => format!(
            "**Schedule:** {parent_name}\n**Status:** {state_str}\n**Result:** {result_str}"
        ),
    }
}

fn parse_priority_str(s: &str) -> NotificationPriority {
    match s {
        "warning" => NotificationPriority::Warning,
        "urgent" => NotificationPriority::Urgent,
        "critical" => NotificationPriority::Critical,
        _ => NotificationPriority::Info,
    }
}
