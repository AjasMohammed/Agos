use crate::kernel::Kernel;
use agentos_bus::BusMessage;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

/// Identifies which kernel subsystem task is running, for targeted restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskKind {
    Acceptor,
    Executor,
    TimeoutChecker,
    Scheduler,
    EventDispatcher,
    ToolLifecycleListener,
    CommNotificationListener,
    ScheduleNotificationListener,
    ArbiterNotificationListener,
    HealthMonitor,
    Consolidation,
    ChannelInboundListener,
}

impl std::fmt::Display for TaskKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskKind::Acceptor => write!(f, "Acceptor"),
            TaskKind::Executor => write!(f, "TaskExecutor"),
            TaskKind::TimeoutChecker => write!(f, "TimeoutChecker"),
            TaskKind::Scheduler => write!(f, "AgentdScheduler"),
            TaskKind::EventDispatcher => write!(f, "EventDispatcher"),
            TaskKind::ToolLifecycleListener => write!(f, "ToolLifecycleListener"),
            TaskKind::CommNotificationListener => write!(f, "CommNotificationListener"),
            TaskKind::ScheduleNotificationListener => write!(f, "ScheduleNotificationListener"),
            TaskKind::ArbiterNotificationListener => write!(f, "ArbiterNotificationListener"),
            TaskKind::HealthMonitor => write!(f, "HealthMonitor"),
            TaskKind::Consolidation => write!(f, "Consolidation"),
            TaskKind::ChannelInboundListener => write!(f, "ChannelInboundListener"),
        }
    }
}

/// Maximum restarts per task within the restart window before declaring degraded.
const MAX_RESTARTS: u32 = 5;
/// Window in which MAX_RESTARTS is counted (seconds).
const RESTART_WINDOW_SECS: u64 = 60;
/// Base delay for exponential backoff (milliseconds).
const BACKOFF_BASE_MS: u64 = 500;
/// Maximum delay between restarts (milliseconds).
const BACKOFF_MAX_MS: u64 = 30_000;

/// Per-subsystem restart tracking with exponential backoff and circuit breaker state.
struct SubsystemState {
    attempt: u32,
    window_start: std::time::Instant,
    circuit_open: bool,
}

impl SubsystemState {
    fn new() -> Self {
        Self {
            attempt: 0,
            window_start: std::time::Instant::now(),
            circuit_open: false,
        }
    }
}

impl TaskKind {
    /// Returns true for subsystems whose budget exhaustion should shut down the entire kernel.
    fn is_critical(&self) -> bool {
        matches!(
            self,
            TaskKind::Acceptor
                | TaskKind::Executor
                | TaskKind::TimeoutChecker
                | TaskKind::EventDispatcher
        )
    }
}

/// Calculate the backoff delay for a given attempt number and task name seed.
/// Uses exponential backoff with per-task jitter: min(base * 2^attempt, max) + jitter(task, attempt)
fn calculate_restart_delay(attempt: u32, task_seed: u64) -> Duration {
    let base = BACKOFF_BASE_MS.saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX));
    let clamped = base.min(BACKOFF_MAX_MS);
    // Deterministic jitter varies by both task identity and attempt, preventing thundering herd
    let jitter_ms = task_seed
        .wrapping_add(attempt as u64)
        .wrapping_mul(2654435761)
        % 500;
    Duration::from_millis(clamped + jitter_ms)
}

/// Compute a stable seed from a task name for use in jitter calculations.
fn task_name_seed(name: &str) -> u64 {
    // FNV-1a hash — no dependencies needed
    let mut hash: u64 = 14695981039346656037;
    for byte in name.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

impl Kernel {
    /// Re-read `[tools.host_package]` from the config file on disk and push
    /// the fresh allowlist + manager priority list into the live
    /// `HostPackagePolicy` handle. Called from the `ConfigWatcher` reload
    /// arm so revocations and additions take effect on the next tool call
    /// — without this the privileged tool would keep running against the
    /// boot-time snapshot until the kernel restarts.
    ///
    /// Allowlist + managers are swapped atomically (single `RwLock` write)
    /// so an in-flight `host-package-install` cannot observe a torn view
    /// where the allowlist is fresh but the managers list is stale.
    ///
    /// Failures are logged but never propagated; a malformed config file
    /// must not crash the kernel. The previous policy stays in effect.
    pub(crate) async fn reload_host_package_policy(&self) {
        let config = match crate::config::load_config(&self.config_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    path = %self.config_path.display(),
                    "Config reload: failed to re-parse config file; \
                     keeping previous host-package-install policy"
                );
                let _ = self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::KernelConfigChanged,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "subsystem": "host_package",
                        "outcome": "reload_failed",
                        "error": e.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Error,
                    reversible: false,
                    rollback_ref: None,
                });
                return;
            }
        };
        let hp = &config.tools.host_package;
        let prev = self
            .host_package_policy
            .replace(hp.allowlist.clone(), hp.managers.clone())
            .await;

        // Compute a by-name diff so the audit entry tells operators
        // exactly which packages were added or removed (review finding
        // I3). Set semantics intentionally — duplicates collapse.
        use std::collections::HashSet;
        let prev_set: HashSet<&String> = prev.allowlist.iter().collect();
        let new_set: HashSet<&String> = hp.allowlist.iter().collect();
        let added: Vec<String> = new_set
            .difference(&prev_set)
            .map(|s| (*s).clone())
            .collect();
        let removed: Vec<String> = prev_set
            .difference(&new_set)
            .map(|s| (*s).clone())
            .collect();

        tracing::info!(
            prev_allowlist_size = prev.allowlist.len(),
            new_allowlist_size = hp.allowlist.len(),
            added = ?added,
            removed = ?removed,
            "host-package-install policy hot-reloaded"
        );
        let _ = self.audit.append(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type: agentos_audit::AuditEventType::KernelConfigChanged,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "subsystem": "host_package",
                "outcome": "reload_ok",
                "allowlist_size": hp.allowlist.len(),
                "managers_size": hp.managers.len(),
                "added_packages": added,
                "removed_packages": removed,
                "enabled": hp.enabled,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });
    }

    /// Spawn a kernel subsystem task into the JoinSet, returning its AbortHandle for ID tracking.
    fn spawn_task(
        join_set: &mut JoinSet<TaskKind>,
        kind: TaskKind,
        kernel: Arc<Kernel>,
    ) -> tokio::task::AbortHandle {
        match kind {
            TaskKind::Acceptor => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            result = kernel.bus.accept() => {
                                match result {
                                    Ok(conn) => {
                                        let kernel = kernel.clone();
                                        tokio::spawn(async move {
                                            kernel.handle_connection(conn).await;
                                        });
                                    }
                                    Err(e) => {
                                        tracing::error!("Bus accept error: {}", e);
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::Acceptor
                })
            }
            TaskKind::Executor => join_set.spawn(async move {
                kernel.task_executor_loop().await;
                TaskKind::Executor
            }),
            TaskKind::TimeoutChecker => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    let mut tick: u64 = 0;
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                                let timed_out_tasks = kernel.scheduler.check_timeouts().await;
                                for timed_out in timed_out_tasks {
                                    kernel
                                        .emit_event(
                                            agentos_types::EventType::TaskTimedOut,
                                            agentos_types::EventSource::TaskScheduler,
                                            agentos_types::EventSeverity::Warning,
                                            serde_json::json!({
                                                "task_id": timed_out.task_id.to_string(),
                                                "agent_id": timed_out.agent_id.to_string(),
                                                "timeout_seconds": timed_out.timeout_seconds,
                                                "elapsed_seconds": timed_out.elapsed_seconds,
                                            }),
                                            0,
                                        )
                                        .await;
                                    kernel
                                        .emit_event(
                                            agentos_types::EventType::TaskFailed,
                                            agentos_types::EventSource::TaskScheduler,
                                            agentos_types::EventSeverity::Warning,
                                            serde_json::json!({
                                                "task_id": timed_out.task_id.to_string(),
                                                "agent_id": timed_out.agent_id.to_string(),
                                                "reason": "task_timed_out",
                                                "error": format!(
                                                    "Task exceeded timeout ({}s > {}s)",
                                                    timed_out.elapsed_seconds,
                                                    timed_out.timeout_seconds
                                                ),
                                            }),
                                            0,
                                        )
                                        .await;
                                    kernel
                                        .background_pool
                                        .fail(
                                            &timed_out.task_id,
                                            format!(
                                                "Task timed out after {}s (limit {}s)",
                                                timed_out.elapsed_seconds, timed_out.timeout_seconds
                                            ),
                                        )
                                        .await;
                                    let waiters = kernel
                                        .scheduler
                                        .complete_dependency(timed_out.task_id)
                                        .await;
                                    for waiter_id in waiters {
                                        if let Err(e) = kernel.scheduler.requeue(&waiter_id).await {
                                            tracing::warn!(error = %e, waiter_id = %waiter_id, "Requeue failed after timeout — waiter will timeout naturally");
                                        }
                                    }
                                    // Send timeout notification to user inbox (root tasks only).
                                    if kernel.config.notifications.notify_on_task_failed {
                                        if let Some(task) = kernel.scheduler.get_task(&timed_out.task_id).await {
                                            if crate::kernel::Kernel::is_root_task(&task) {
                                                let summary = format!(
                                                    "Task timed out after {}s (limit {}s)",
                                                    timed_out.elapsed_seconds,
                                                    timed_out.timeout_seconds
                                                );
                                                let (last_tool, last_iter, obs_iter, obs_tools) =
                                                    kernel.gather_task_progress(&timed_out.task_id).await;
                                                let failure = crate::task_completion::FailureDetails {
                                                    reason: "timeout".to_string(),
                                                    error_chain: vec![summary.clone()],
                                                    last_tool,
                                                    last_iteration: last_iter,
                                                };
                                                kernel
                                                    .send_completion_notification(
                                                        &task,
                                                        agentos_types::TaskOutcome::TimedOut,
                                                        &summary,
                                                        obs_tools,
                                                        obs_iter,
                                                        timed_out.elapsed_seconds * 1000,
                                                        agentos_types::TraceID::new(),
                                                        Some(failure),
                                                    )
                                                    .await;
                                            }
                                        }
                                    }
                                    kernel.cleanup_task_subscriptions(&timed_out.task_id).await;
                                    // Release context window, intent validator state, and resource
                                    // locks held by this task — the timeout checker is the terminal
                                    // authority; execute_task_sync will see the terminal state and
                                    // skip its own cleanup path for these.
                                    kernel.context_manager.remove_context(&timed_out.task_id).await;
                                    kernel.intent_validator.remove_task(&timed_out.task_id).await;
                                    kernel.resource_arbiter.release_all_for_agent(timed_out.agent_id).await;
                                }

                                // Sweep expired RPC calls (Phase 7)
                                let expired_rpcs = kernel.rpc_manager.sweep_expired().await;
                                for rpc_task_id in &expired_rpcs {
                                    kernel
                                        .emit_event(
                                            agentos_types::EventType::AgentRpcCallTimedOut,
                                            agentos_types::EventSource::AgentMessageBus,
                                            agentos_types::EventSeverity::Warning,
                                            serde_json::json!({
                                                "rpc_task_id": rpc_task_id.to_string(),
                                                "reason": "rpc_timeout",
                                            }),
                                            0,
                                        )
                                        .await;
                                }

                                // Sweep expired resource locks (Spec §8)
                                kernel.resource_arbiter.sweep_expired().await;

                                // Sweep expired vault proxy tokens (Spec §3)
                                kernel.vault.sweep_expired_proxy_tokens().await;

                                // Sweep expired agent inbox items
                                if let Err(e) = kernel.agent_inbox.sweep_expired().await {
                                    tracing::warn!(error = %e, "Agent inbox sweep failed");
                                }

                                // Sweep expired agent messages
                                if let Err(e) = kernel.agent_message_inbox.sweep_expired().await {
                                    tracing::warn!(error = %e, "Agent message inbox sweep failed");
                                }

                                // Sweep expired escalations — auto-deny (Spec §12)
                                let expired_escalations = kernel.escalation_manager.sweep_expired().await;
                                for (esc_id, task_id, agent_id, blocking, auto_action) in &expired_escalations {
                                    if matches!(auto_action, crate::escalation::AutoAction::Deny) {
                                        if let Some(escalation) =
                                            kernel.escalation_manager.get(*esc_id).await
                                        {
                                            let is_device_access = escalation
                                                .metadata
                                                .get("kind")
                                                .and_then(serde_json::Value::as_str)
                                                == Some("device_access");
                                            if is_device_access {
                                                if let Some(device_id) = escalation
                                                    .metadata
                                                    .get("device_id")
                                                    .and_then(serde_json::Value::as_str)
                                                {
                                                    let deny_result = match kernel
                                                        .hardware_registry
                                                        .get_device(device_id)
                                                    {
                                                        Some(device)
                                                            if device.status
                                                                == agentos_hal::DeviceStatus::Approved =>
                                                        {
                                                            kernel.hardware_registry.deny_for_agent(
                                                                device_id,
                                                                *agent_id,
                                                            )
                                                        }
                                                        Some(_) => kernel
                                                            .hardware_registry
                                                            .set_device_status(
                                                                device_id,
                                                                agentos_hal::DeviceStatus::Quarantined,
                                                            ),
                                                        None => Err(agentos_types::AgentOSError::HalError(format!(
                                                            "Device '{}' not found while expiring escalation",
                                                            device_id
                                                        ))),
                                                    };

                                                    if deny_result.is_ok() {
                                                        kernel.audit_log(agentos_audit::AuditEntry {
                                                            timestamp: chrono::Utc::now(),
                                                            trace_id: agentos_types::TraceID::new(),
                                                            event_type: agentos_audit::AuditEventType::DeviceAccessDenied,
                                                            agent_id: Some(*agent_id),
                                                            task_id: Some(*task_id),
                                                            tool_id: None,
                                                            details: serde_json::json!({
                                                                "device_id": device_id,
                                                                "reason": "device access escalation expired",
                                                                "escalation_id": esc_id,
                                                            }),
                                                            severity: agentos_audit::AuditSeverity::Warn,
                                                            reversible: false,
                                                            rollback_ref: None,
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    let mut task_resumed = false;

                                    if *blocking {
                                        match auto_action {
                                            crate::escalation::AutoAction::Approve => {
                                                match kernel.scheduler.requeue(task_id).await {
                                                    Ok(()) => {
                                                        task_resumed = true;
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(
                                                            task_id = %task_id,
                                                            error = %e,
                                                            "Failed to requeue task after escalation auto-approve; failing task"
                                                        );
                                                        let can_transition_failed = kernel
                                                            .scheduler
                                                            .get_task(task_id)
                                                            .await
                                                            .map(|t| {
                                                                !matches!(
                                                                    t.state,
                                                                    agentos_types::TaskState::Complete
                                                                        | agentos_types::TaskState::Failed
                                                                        | agentos_types::TaskState::Cancelled
                                                                )
                                                            })
                                                            .unwrap_or(false);
                                                        if can_transition_failed {
                                                            let transitioned = kernel
                                                                .scheduler
                                                                .update_state_if_not_terminal(
                                                                    task_id,
                                                                    agentos_types::TaskState::Failed,
                                                                )
                                                                .await
                                                                .unwrap_or(false);
                                                            if transitioned {
                                                                kernel
                                                                    .background_pool
                                                                    .fail(task_id, "Escalation auto-approve requeue failed".to_string())
                                                                    .await;
                                                                kernel
                                                                    .emit_event(
                                                                        agentos_types::EventType::TaskFailed,
                                                                        agentos_types::EventSource::TaskScheduler,
                                                                        agentos_types::EventSeverity::Warning,
                                                                        serde_json::json!({
                                                                            "task_id": task_id.to_string(),
                                                                            "agent_id": agent_id.to_string(),
                                                                            "reason": "escalation_auto_approve_requeue_failed",
                                                                            "error": format!("Escalation auto-approve requeue failed: {}", e),
                                                                        }),
                                                                        0,
                                                                    )
                                                                    .await;
                                                                let waiters =
                                                                    kernel.scheduler.complete_dependency(*task_id).await;
                                                                for waiter_id in waiters {
                                                                    if let Err(e) = kernel.scheduler.requeue(&waiter_id).await {
                                            tracing::warn!(error = %e, waiter_id = %waiter_id, "Requeue failed after timeout — waiter will timeout naturally");
                                        }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            crate::escalation::AutoAction::Deny => {
                                                let can_transition_failed = kernel
                                                    .scheduler
                                                    .get_task(task_id)
                                                    .await
                                                    .map(|t| {
                                                        !matches!(
                                                            t.state,
                                                            agentos_types::TaskState::Complete
                                                                | agentos_types::TaskState::Failed
                                                                | agentos_types::TaskState::Cancelled
                                                        )
                                                    })
                                                    .unwrap_or(false);
                                                if can_transition_failed {
                                                    let transitioned = kernel
                                                        .scheduler
                                                        .update_state_if_not_terminal(
                                                            task_id,
                                                            agentos_types::TaskState::Failed,
                                                        )
                                                        .await
                                                        .unwrap_or(false);
                                                    if transitioned {
                                                        // Clean up context and intent history for the
                                                        // failed task to prevent unbounded memory growth.
                                                        kernel.context_manager.remove_context(task_id).await;
                                                        kernel.intent_validator.remove_task(task_id).await;
                                                        kernel
                                                            .background_pool
                                                            .fail(task_id, "Escalation expired and auto-denied".to_string())
                                                            .await;
                                                        kernel
                                                            .emit_event(
                                                                agentos_types::EventType::TaskFailed,
                                                                agentos_types::EventSource::TaskScheduler,
                                                                agentos_types::EventSeverity::Warning,
                                                                serde_json::json!({
                                                                    "task_id": task_id.to_string(),
                                                                    "agent_id": agent_id.to_string(),
                                                                    "reason": "escalation_expired",
                                                                    "error": "Escalation expired and auto-denied",
                                                                }),
                                                                0,
                                                            )
                                                            .await;
                                                        let waiters =
                                                            kernel.scheduler.complete_dependency(*task_id).await;
                                                        for waiter_id in waiters {
                                                            if let Err(e) = kernel.scheduler.requeue(&waiter_id).await {
                                            tracing::warn!(error = %e, waiter_id = %waiter_id, "Requeue failed after timeout — waiter will timeout naturally");
                                        }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    let mut details = serde_json::json!({
                                        "escalation_id": esc_id,
                                        "auto_action": format!("{:?}", auto_action).to_lowercase(),
                                        "reason": "escalation_expired",
                                        "blocking": blocking,
                                    });
                                    if *blocking {
                                        details["task_resumed"] = serde_json::json!(task_resumed);
                                    }

                                    kernel.audit_log(agentos_audit::AuditEntry {
                                        timestamp: chrono::Utc::now(),
                                        trace_id: agentos_types::TraceID::new(),
                                        event_type: if *blocking && task_resumed {
                                            agentos_audit::AuditEventType::TaskStateChanged
                                        } else if *blocking
                                            && matches!(auto_action, crate::escalation::AutoAction::Approve)
                                        {
                                            agentos_audit::AuditEventType::TaskFailed
                                        } else if matches!(auto_action, crate::escalation::AutoAction::Deny) {
                                            agentos_audit::AuditEventType::ActionForbidden
                                        } else {
                                            agentos_audit::AuditEventType::RiskEscalation
                                        },
                                        agent_id: Some(*agent_id),
                                        task_id: Some(*task_id),
                                        tool_id: None,
                                        details,
                                        severity: if *blocking
                                            && matches!(auto_action, crate::escalation::AutoAction::Approve)
                                            && !task_resumed
                                        {
                                            agentos_audit::AuditSeverity::Error
                                        } else if matches!(auto_action, crate::escalation::AutoAction::Deny) {
                                            agentos_audit::AuditSeverity::Warn
                                        } else {
                                            agentos_audit::AuditSeverity::Info
                                        },
                                        reversible: false,
                                        rollback_ref: None,
                                    });
                                }

                                // Sweep expired snapshots every ~10 minutes (60 ticks × 10s)
                                tick += 1;
                                if tick.is_multiple_of(60) {
                                    kernel.sweep_expired_snapshots(
                                        Duration::from_secs(72 * 3600), // 72h (Spec §5)
                                    );

                                    // Prune checkpoints older than 72h.
                                    {
                                        let cp_store = kernel.checkpoint_store.clone();
                                        tokio::spawn(async move {
                                            match cp_store.prune_older_than(chrono::Duration::hours(72)).await {
                                                Ok(0) => {}
                                                Ok(n) => {
                                                    tracing::info!(pruned = n, "Pruned {} expired checkpoints", n);
                                                }
                                                Err(e) => {
                                                    tracing::warn!(error = %e, "Checkpoint pruning failed");
                                                }
                                            }
                                        });
                                    }

                                    // Sweep expired OAuth pending flows (10min TTL)
                                    if let Err(e) = kernel.vault.oauth_store().sweep_expired_flows().await {
                                        tracing::warn!(error = %e, "OAuth pending flow sweep failed");
                                    }

                                    // Sweep expired notification waiters (blocking ask_user
                                    // questions whose timeout has fired). Fires auto_action
                                    // and wakes blocked tasks (Architecture Review ISSUE-6).
                                    kernel.notification_router.sweep_expired_waiters().await;

                                    // Evict terminal background tasks older than 1 hour to
                                    // prevent unbounded pool growth for long-running kernels.
                                    kernel.background_pool.evict_terminal(3600).await;

                                    // Drop chat-session dedup caches untouched for >24h.
                                    let evicted = kernel
                                        .sweep_chat_session_dedup(Duration::from_secs(24 * 3600))
                                        .await;
                                    if evicted > 0 {
                                        tracing::info!(
                                            evicted,
                                            "Pruned {evicted} idle chat-session dedup caches"
                                        );
                                    }

                                    // Prune old audit log entries if a rotation limit is set
                                    let max_entries = kernel.config.audit.max_audit_entries;
                                    if max_entries > 0 {
                                        match kernel.audit.prune_old_entries(max_entries) {
                                            Ok(0) => {}
                                            Ok(n) => tracing::info!(
                                                pruned = n,
                                                max_entries,
                                                "Audit log rotated: pruned old entries"
                                            ),
                                            Err(e) => tracing::warn!(
                                                error = %e,
                                                "Audit log rotation failed"
                                            ),
                                        }
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::TimeoutChecker
                })
            }
            TaskKind::Scheduler => join_set.spawn(async move {
                kernel.agentd_loop().await;
                TaskKind::Scheduler
            }),
            TaskKind::EventDispatcher => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    let mut rx = kernel.event_receiver.lock().await;
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            event = rx.recv() => {
                                match event {
                                    Some(event) => kernel.process_event(event).await,
                                    None => {
                                        tracing::warn!("Event channel closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::EventDispatcher
                })
            }
            TaskKind::ToolLifecycleListener => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    let mut rx = kernel.tool_lifecycle_receiver.lock().await;
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            event = rx.recv() => {
                                match event {
                                    Some(event) => kernel.process_tool_lifecycle_event(event).await,
                                    None => {
                                        tracing::warn!("Tool lifecycle channel closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::ToolLifecycleListener
                })
            }
            TaskKind::CommNotificationListener => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    let mut rx = kernel.comm_notification_receiver.lock().await;
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            notif = rx.recv() => {
                                match notif {
                                    Some(n) => kernel.process_comm_notification(n).await,
                                    None => {
                                        tracing::warn!("Comm notification channel closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::CommNotificationListener
                })
            }
            TaskKind::ScheduleNotificationListener => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    let mut rx = kernel.schedule_notification_receiver.lock().await;
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            notif = rx.recv() => {
                                match notif {
                                    Some(n) => kernel.process_schedule_notification(n).await,
                                    None => {
                                        tracing::warn!("Schedule notification channel closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::ScheduleNotificationListener
                })
            }
            TaskKind::ArbiterNotificationListener => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    let mut rx = kernel.arbiter_notification_receiver.lock().await;
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            notif = rx.recv() => {
                                match notif {
                                    Some(n) => kernel.process_arbiter_notification(n).await,
                                    None => {
                                        tracing::warn!("Arbiter notification channel closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::ArbiterNotificationListener
                })
            }
            TaskKind::HealthMonitor => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    crate::health_monitor::run_health_monitor(kernel, token).await;
                    TaskKind::HealthMonitor
                })
            }
            TaskKind::Consolidation => {
                let token = kernel.cancellation_token.clone();
                let engine = kernel.consolidation_engine.clone();
                join_set.spawn(async move {
                    // If consolidation is disabled in config, idle until shutdown.
                    if !engine.is_enabled() {
                        token.cancelled().await;
                        return TaskKind::Consolidation;
                    }
                    // Defer the first tick by a full period so the kernel finishes
                    // booting before any consolidation work begins. Using interval_at
                    // also avoids a spurious immediate tick on supervised restarts.
                    let start = tokio::time::Instant::now() + Duration::from_secs(1800);
                    let mut interval = tokio::time::interval_at(start, Duration::from_secs(1800));
                    // Skip missed ticks — catching up with burst consolidation on a
                    // busy system would waste resources; next scheduled tick is fine.
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            _ = interval.tick() => {
                                match engine.run_cycle().await {
                                    Ok(report) if report.created > 0 => {
                                        tracing::info!(
                                            patterns = report.patterns_found,
                                            created = report.created,
                                            skipped = report.skipped_existing,
                                            "Consolidation cycle completed"
                                        );
                                    }
                                    Ok(_) => {
                                        tracing::debug!("Consolidation cycle: no new procedures");
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, "Consolidation cycle failed");
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::Consolidation
                })
            }
            TaskKind::ChannelInboundListener => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    let mut rx = kernel.channel_manager_rx.lock().await;
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            msg = rx.recv() => {
                                match msg {
                                    Some(inbound) => {
                                        tracing::debug!(
                                            channel_type = %inbound.channel_type,
                                            instance_id = %inbound.channel_instance_id,
                                            "Inbound channel message received"
                                        );
                                        // TODO: Route to bound agent based on
                                        // channel_instance_id -> agent mapping.
                                    }
                                    None => {
                                        tracing::warn!("Channel inbound channel closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    TaskKind::ChannelInboundListener
                })
            }
        }
    }

    /// Spawn a task and register its tokio task ID for panic identification.
    fn spawn_tracked_task(
        join_set: &mut JoinSet<TaskKind>,
        task_map: &mut std::collections::HashMap<tokio::task::Id, TaskKind>,
        kind: TaskKind,
        kernel: Arc<Kernel>,
    ) {
        let handle = Self::spawn_task(join_set, kind, kernel);
        task_map.insert(handle.id(), kind);
    }

    /// The main supervised run loop.
    ///
    /// Spawns 11 core tasks (acceptor, executor, timeout checker, scheduler, event dispatcher,
    /// tool lifecycle listener, comm notification listener, schedule notification listener,
    /// arbiter notification listener, health monitor, consolidation) and monitors them via a JoinSet. If any task
    /// panics or exits unexpectedly, it is restarted automatically. If a task exceeds
    /// MAX_RESTARTS within RESTART_WINDOW_SECS, the kernel logs a degraded status and shuts
    /// down so the container orchestrator can restart the process cleanly.
    pub async fn run(self: Arc<Self>) -> Result<(), anyhow::Error> {
        let mut join_set = JoinSet::new();
        // Map tokio task IDs to TaskKind for targeted panic recovery
        let mut task_id_map: std::collections::HashMap<tokio::task::Id, TaskKind> =
            std::collections::HashMap::new();

        // Track per-subsystem restart state (backoff + circuit breaker)
        let mut subsystem_states: std::collections::HashMap<String, SubsystemState> =
            std::collections::HashMap::new();

        // Pending delayed restarts: (fire_at, TaskKind) — avoids blocking the supervisor loop.
        let mut pending_restarts: Vec<(tokio::time::Instant, TaskKind)> = Vec::new();

        // Spawn all 12 core tasks
        let all_kinds = [
            TaskKind::Acceptor,
            TaskKind::Executor,
            TaskKind::TimeoutChecker,
            TaskKind::Scheduler,
            TaskKind::EventDispatcher,
            TaskKind::ToolLifecycleListener,
            TaskKind::CommNotificationListener,
            TaskKind::ScheduleNotificationListener,
            TaskKind::ArbiterNotificationListener,
            TaskKind::HealthMonitor,
            TaskKind::Consolidation,
            TaskKind::ChannelInboundListener,
        ];

        for kind in &all_kinds {
            Self::spawn_tracked_task(&mut join_set, &mut task_id_map, *kind, self.clone());
        }

        // Start config file watcher — sends on reload_rx when the loaded config file changes.
        // The watcher is kept alive by holding it in a local variable.
        let (reload_tx, mut reload_rx) = tokio::sync::mpsc::channel::<()>(4);
        let config_path = self.config_path.clone();
        let _config_watcher =
            match crate::config_watcher::ConfigWatcher::start(config_path, reload_tx) {
                Ok(w) => {
                    tracing::info!("Hot config reload enabled");
                    Some(w)
                }
                Err(e) => {
                    tracing::warn!("Config watcher unavailable (hot reload disabled): {}", e);
                    None
                }
            };

        // Install Prometheus metrics recorder and start health/readiness/metrics HTTP server
        if let Some(prom_handle) = crate::health::install_prometheus_recorder() {
            if let Err(e) = crate::health::start_health_server(self.clone(), prom_handle).await {
                tracing::warn!(error = %e, "Failed to start health server, continuing without it");
            }
        }

        tracing::info!("Kernel supervisor started with {} tasks", all_kinds.len());
        // Note: the docstring above still says "11 core tasks" — updated to 12 with ChannelInboundListener.

        // Signal systemd that the kernel is ready to accept connections.
        // No-op when NOTIFY_SOCKET is not set (Docker, direct invocation, tests).
        crate::sd_notify::notify_ready();

        loop {
            // Compute deadline for the earliest pending restart (None = no pending restarts).
            let next_restart_at: Option<tokio::time::Instant> =
                pending_restarts.iter().map(|(t, _)| *t).min();

            tokio::select! {
                _ = self.cancellation_token.cancelled() => {
                    tracing::info!("Kernel shutdown requested, stopping supervisor");

                    // Log any tasks that are still in-flight so operators know what was lost.
                    let in_flight: Vec<_> = self
                        .scheduler
                        .list_tasks()
                        .await
                        .into_iter()
                        .filter(|t| {
                            matches!(
                                t.state,
                                agentos_types::TaskState::Running | agentos_types::TaskState::Waiting
                            )
                        })
                        .collect();
                    if in_flight.is_empty() {
                        tracing::info!("Shutdown: no in-flight tasks");
                    } else {
                        tracing::warn!(
                            count = in_flight.len(),
                            "Shutdown: {} task(s) abandoned mid-execution",
                            in_flight.len()
                        );
                        for t in &in_flight {
                            tracing::warn!(
                                task_id = %t.id,
                                agent_id = %t.agent_id,
                                state = ?t.state,
                                "Abandoned task on shutdown"
                            );
                        }
                    }

                    join_set.abort_all();
                    self.channel_listener_registry.stop_all().await;
                    // Fire Shutdown hook before final audit entry so hooks can flush state.
                    self.hook_registry
                        .fire(&agentos_types::HookEvent::Shutdown)
                        .await;
                    self.audit_shutdown("cancellation_token", agentos_audit::AuditSeverity::Info);
                    break;
                }

                // Hot config reload: fired when the config file changes on disk.
                // Drains all queued signals first to avoid redundant reloads on rapid saves.
                // NOTE: Full live-reload of subsystems (LLM provider, bus, etc.) is a future
                // enhancement. For now this logs the event and signals the audit log so operators
                // know a config change occurred without restarting the kernel.
                Some(()) = reload_rx.recv() => {
                    while reload_rx.try_recv().is_ok() {}
                    tracing::info!("Config file changed on disk");

                    // Hot-reload the `[tools.host_package]` allowlist + manager
                    // list. Revocations take effect on the next tool call —
                    // critical for a privileged tool where leaving a stale
                    // allowlist in place after the operator removes a package
                    // would be a security gap.
                    self.reload_host_package_policy().await;

                    // Fire ConfigReloaded hook — lets hooks (audit, metrics) observe the change.
                    self.hook_registry
                        .fire(&agentos_types::HookEvent::ConfigReloaded)
                        .await;
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: agentos_types::TraceID::new(),
                        event_type: agentos_audit::AuditEventType::KernelConfigChanged,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "source": "config_watcher" }),
                        severity: agentos_audit::AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    });
                }

                // Fire any pending restarts whose deadline has passed.
                // Using std::future::pending() when the queue is empty keeps this arm dormant.
                _ = async {
                    if let Some(at) = next_restart_at {
                        tokio::time::sleep_until(at).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    // Drain first, then spawn — avoids spawning inside an iteration.
                    let now = tokio::time::Instant::now();
                    let ready: Vec<TaskKind> = pending_restarts
                        .iter()
                        .filter(|(at, _)| *at <= now)
                        .map(|(_, kind)| *kind)
                        .collect();
                    pending_restarts.retain(|(at, _)| *at > now);
                    for kind in ready {
                        Self::spawn_tracked_task(
                            &mut join_set,
                            &mut task_id_map,
                            kind,
                            self.clone(),
                        );
                    }
                }

                next = join_set.join_next() => match next {
                    Some(Ok(kind)) => {
                        // Task completed normally — unexpected for long-running loops.
                        // Remove the now-stale task ID entry to prevent map growth and
                        // potential misidentification if a new task reuses the same ID.
                        task_id_map.retain(|_, v| *v != kind);

                        // If the cancellation token fired, this exit is expected (the task
                        // detected shutdown and returned cleanly). Skip the restart path —
                        // the outer `cancelled()` arm will drain remaining tasks and break.
                        if self.cancellation_token.is_cancelled() {
                            tracing::debug!(task = %kind, "Kernel task exited during shutdown (expected)");
                            continue;
                        }

                        tracing::warn!(task = %kind, "Kernel task exited unexpectedly, restarting");

                        self.audit_log(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: agentos_types::TraceID::new(),
                            event_type: agentos_audit::AuditEventType::KernelSubsystemRestarted,
                            agent_id: None,
                            task_id: None,
                            tool_id: None,
                            details: serde_json::json!({
                                "event": "task_restart",
                                "task": kind.to_string(),
                                "reason": "normal_exit",
                            }),
                            severity: agentos_audit::AuditSeverity::Warn,
                            reversible: false,
                            rollback_ref: None,
                        });

                        match self
                            .check_restart_with_backoff(&mut subsystem_states, &kind.to_string())
                        {
                            Some(delay) => {
                                let attempt = subsystem_states
                                    .get(&kind.to_string())
                                    .map(|s| s.attempt)
                                    .unwrap_or(0);
                                tracing::info!(
                                    task = %kind,
                                    delay_ms = delay.as_millis() as u64,
                                    attempt,
                                    "Scheduling task restart with backoff"
                                );
                                pending_restarts
                                    .push((tokio::time::Instant::now() + delay, kind));
                            }
                            None => {
                                if kind.is_critical() {
                                    self.emit_event(
                                        agentos_types::EventType::KernelSubsystemError,
                                        agentos_types::EventSource::InferenceKernel,
                                        agentos_types::EventSeverity::Critical,
                                        serde_json::json!({
                                            "task_kind": kind.to_string(),
                                            "reason": "restart_budget_exceeded",
                                            "max_restarts": MAX_RESTARTS,
                                        }),
                                        0,
                                    )
                                    .await;
                                    tracing::error!(
                                        task = %kind,
                                        "Critical task exceeded restart budget, kernel shutting down"
                                    );
                                    self.audit_shutdown(
                                        "restart_budget_exhausted",
                                        agentos_audit::AuditSeverity::Error,
                                    );
                                    break;
                                } else {
                                    tracing::error!(
                                        task = %kind,
                                        "Non-critical task exceeded restart budget, marking subsystem degraded"
                                    );
                                    self.emit_event(
                                        agentos_types::EventType::KernelSubsystemError,
                                        agentos_types::EventSource::InferenceKernel,
                                        agentos_types::EventSeverity::Warning,
                                        serde_json::json!({
                                            "task_kind": kind.to_string(),
                                            "reason": "restart_budget_exceeded_degraded",
                                            "max_restarts": MAX_RESTARTS,
                                        }),
                                        0,
                                    )
                                    .await;
                                    // Non-critical: keep the rest of the kernel running
                                }
                            }
                        }
                    }
                    Some(Err(join_error)) => {
                        // Identify the crashed task by its tokio task ID
                        let crashed_task_id = join_error.id();
                        let identified_kind = task_id_map.remove(&crashed_task_id);

                        let task_name = if let Some(kind) = identified_kind {
                            kind.to_string()
                        } else if join_error.is_panic() {
                            "unknown_panic".to_string()
                        } else {
                            "unknown_cancelled".to_string()
                        };

                        // Emit ProcessCrashed for panics
                        if join_error.is_panic() {
                            self.emit_event(
                                agentos_types::EventType::ProcessCrashed,
                                agentos_types::EventSource::InferenceKernel,
                                agentos_types::EventSeverity::Critical,
                                serde_json::json!({
                                    "task_kind": task_name,
                                    "panic": true,
                                    "error": format!("{:?}", join_error),
                                }),
                                0,
                            )
                            .await;
                            tracing::error!(
                                task = %task_name,
                                "Kernel task panicked: {:?}", join_error
                            );
                        } else {
                            tracing::error!(
                                task = %task_name,
                                "Kernel task cancelled: {:?}", join_error
                            );
                        }

                        self.audit_log(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: agentos_types::TraceID::new(),
                            event_type: agentos_audit::AuditEventType::KernelSubsystemRestarted,
                            agent_id: None,
                            task_id: None,
                            tool_id: None,
                            details: serde_json::json!({
                                "event": "task_panic",
                                "task": task_name,
                                "error": format!("{:?}", join_error),
                            }),
                            severity: agentos_audit::AuditSeverity::Error,
                            reversible: false,
                            rollback_ref: None,
                        });

                        match self
                            .check_restart_with_backoff(&mut subsystem_states, &task_name)
                        {
                            Some(delay) => {
                                if let Some(kind) = identified_kind {
                                    let attempt = subsystem_states
                                        .get(&task_name)
                                        .map(|s| s.attempt)
                                        .unwrap_or(0);
                                    tracing::info!(
                                        task = %kind,
                                        delay_ms = delay.as_millis() as u64,
                                        attempt,
                                        "Scheduling crashed task restart with backoff"
                                    );
                                    pending_restarts
                                        .push((tokio::time::Instant::now() + delay, kind));
                                } else {
                                    // Fallback: unidentified crash — restart all supervised tasks.
                                    // Respect the cancellation token during the backoff sleep so
                                    // a shutdown request is not delayed up to BACKOFF_MAX_MS.
                                    tracing::warn!(
                                        "Could not identify crashed task, restarting all supervised tasks"
                                    );
                                    tokio::select! {
                                        _ = self.cancellation_token.cancelled() => {
                                            join_set.abort_all();
                                            self.audit_shutdown(
                                                "cancellation_token",
                                                agentos_audit::AuditSeverity::Info,
                                            );
                                            break;
                                        }
                                        _ = tokio::time::sleep(delay) => {
                                            join_set.abort_all();
                                            while join_set.join_next().await.is_some() {}
                                            task_id_map.clear();
                                            subsystem_states.clear();
                                            pending_restarts.clear();
                                            for kind in &all_kinds {
                                                Self::spawn_tracked_task(
                                                    &mut join_set,
                                                    &mut task_id_map,
                                                    *kind,
                                                    self.clone(),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            None => {
                                // Circuit open — decide based on criticality
                                let is_critical =
                                    identified_kind.map(|k| k.is_critical()).unwrap_or(true);
                                if is_critical {
                                    self.emit_event(
                                        agentos_types::EventType::KernelSubsystemError,
                                        agentos_types::EventSource::InferenceKernel,
                                        agentos_types::EventSeverity::Critical,
                                        serde_json::json!({
                                            "task_kind": task_name,
                                            "reason": "restart_budget_exceeded",
                                            "max_restarts": MAX_RESTARTS,
                                        }),
                                        0,
                                    )
                                    .await;
                                    tracing::error!(
                                        task = %task_name,
                                        "Critical task exceeded restart budget, kernel shutting down"
                                    );
                                    self.audit_shutdown(
                                        "restart_budget_exhausted",
                                        agentos_audit::AuditSeverity::Error,
                                    );
                                    break;
                                } else {
                                    tracing::error!(
                                        task = %task_name,
                                        "Non-critical task exceeded restart budget, marking subsystem degraded"
                                    );
                                    self.emit_event(
                                        agentos_types::EventType::KernelSubsystemError,
                                        agentos_types::EventSource::InferenceKernel,
                                        agentos_types::EventSeverity::Warning,
                                        serde_json::json!({
                                            "task_kind": task_name,
                                            "reason": "restart_budget_exceeded_degraded",
                                            "max_restarts": MAX_RESTARTS,
                                        }),
                                        0,
                                    )
                                    .await;
                                    // Non-critical: keep the rest of the kernel running
                                }
                            }
                        }
                    }
                    None => {
                        // All tasks exited — should not happen
                        tracing::error!("All kernel tasks exited, shutting down");
                        self.audit_shutdown(
                            "all_tasks_exited",
                            agentos_audit::AuditSeverity::Error,
                        );
                        break;
                    }
                }
            }
        }

        // Inform systemd on ALL exit paths (clean shutdown, budget exhausted,
        // unexpected task exit, etc.).  Sending STOPPING=1 here gives systemd
        // precise timing: it will not count the post-loop cleanup time against
        // WatchdogSec and will not send SIGKILL before TimeoutStopSec expires.
        crate::sd_notify::notify_stopping();

        Ok(())
    }

    /// Check if a task is within its restart budget and return the backoff delay.
    /// Returns `Some(delay)` if restart is allowed, `None` if the circuit breaker has opened.
    ///
    /// Window reset also clears the circuit breaker — this is the recovery path: a subsystem
    /// that exceeded its budget can be retried after RESTART_WINDOW_SECS has elapsed.
    fn check_restart_with_backoff(
        &self,
        states: &mut std::collections::HashMap<String, SubsystemState>,
        task_name: &str,
    ) -> Option<Duration> {
        let now = std::time::Instant::now();
        let state = states
            .entry(task_name.to_string())
            .or_insert_with(SubsystemState::new);

        // Window reset must happen first — it is also the circuit recovery path.
        // After RESTART_WINDOW_SECS, reset both the attempt counter and the circuit breaker
        // so a subsystem that hit its budget during a transient can recover.
        if now.duration_since(state.window_start) > Duration::from_secs(RESTART_WINDOW_SECS) {
            state.attempt = 0;
            state.window_start = now;
            state.circuit_open = false;
        }

        // If circuit is still open within the current window, reject
        if state.circuit_open {
            return None;
        }

        state.attempt += 1;
        if state.attempt > MAX_RESTARTS {
            state.circuit_open = true;
            return None;
        }

        Some(calculate_restart_delay(
            state.attempt - 1,
            task_name_seed(task_name),
        ))
    }

    /// Handle a single CLI connection with per-connection rate limiting.
    async fn handle_connection(self: &Arc<Self>, mut conn: agentos_bus::BusConnection) {
        // 50 commands per second per connection — configurable via max_intents_per_second
        let mut rate_limiter = crate::rate_limit::RateLimiter::new(50);

        loop {
            let read_result = tokio::select! {
                _ = self.cancellation_token.cancelled() => break,
                result = conn.read() => result,
            };
            match read_result {
                Ok(BusMessage::Command(cmd)) => {
                    // Per-connection rate limit (fast path — no lock needed)
                    if let Err(wait) = rate_limiter.check() {
                        crate::metrics::record_rate_limited();
                        tracing::warn!(
                            wait_ms = wait.as_millis() as u64,
                            "Connection rate limited"
                        );
                        let response = agentos_bus::KernelResponse::Error {
                            message: format!("Rate limited. Retry after {} ms", wait.as_millis()),
                        };
                        if conn
                            .write(&BusMessage::CommandResponse(response))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }

                    // Per-agent rate limit: prevents one agent from bypassing limits via multiple connections
                    if let Some(ref agent_key) = cmd.agent_key() {
                        if let Err(wait) = self.per_agent_rate_limiter.lock().await.check(agent_key)
                        {
                            crate::metrics::record_rate_limited();
                            let rate_err = agentos_types::AgentOSError::RateLimited {
                                detail: format!("retry after {} ms", wait.as_millis()),
                            };
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id: agentos_types::TraceID::new(),
                                event_type: agentos_audit::AuditEventType::ActionForbidden,
                                agent_id: None,
                                task_id: None,
                                tool_id: None,
                                details: serde_json::json!({
                                    "reason": "per_agent_rate_limit_exceeded",
                                    "agent_key": agent_key,
                                    "wait_ms": wait.as_millis(),
                                    "error": rate_err.to_string(),
                                }),
                                severity: agentos_audit::AuditSeverity::Warn,
                                reversible: false,
                                rollback_ref: None,
                            });
                            tracing::warn!(
                                agent_key = %agent_key,
                                wait_ms = wait.as_millis() as u64,
                                "Per-agent rate limit exceeded"
                            );
                            let response = agentos_bus::KernelResponse::Error {
                                message: rate_err.to_string(),
                            };
                            if conn
                                .write(&BusMessage::CommandResponse(response))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                    }

                    let response = self.handle_command(cmd).await;
                    if conn
                        .write(&BusMessage::CommandResponse(response))
                        .await
                        .is_err()
                    {
                        break; // connection closed
                    }
                }
                Err(_) => break, // connection closed
                _ => {}          // ignore unexpected message types
            }
        }
    }

    /// Route a KernelCommand to the appropriate handler.
    async fn handle_command(&self, cmd: agentos_bus::KernelCommand) -> agentos_bus::KernelResponse {
        use agentos_bus::KernelCommand;

        match cmd {
            KernelCommand::ConnectAgent {
                name,
                provider,
                model,
                base_url,
                roles,
                test_mode,
                extra_permissions,
                root,
                skip_health_check,
            } => {
                // Intentionally calls cmd_connect_agent directly (not api_connect_agent):
                // the bus command carries test_mode and extra_permissions which are
                // bus/CLI-only features not exposed by api_connect_agent.
                self.cmd_connect_agent(
                    name,
                    provider,
                    model,
                    base_url,
                    roles,
                    None,
                    None,
                    None,
                    test_mode,
                    extra_permissions,
                    root,
                    skip_health_check,
                )
                .await
            }
            KernelCommand::ListAgents => self.cmd_list_agents().await,
            KernelCommand::SetAgentBaseUrl { name, url } => {
                self.cmd_set_agent_base_url(name, url).await
            }
            KernelCommand::PingLLM {
                provider,
                model,
                base_url,
                agent_name,
            } => {
                self.cmd_ping_llm(provider, model, base_url, agent_name)
                    .await
            }
            KernelCommand::DisconnectAgent { agent_id } => {
                // Route through api_* so any future shared logic (validation, audit hooks)
                // applies to both CLI and REST paths.
                match self.api_disconnect_agent(agent_id).await {
                    Ok(()) => agentos_bus::KernelResponse::Success { data: None },
                    Err(msg) => agentos_bus::KernelResponse::Error { message: msg },
                }
            }
            KernelCommand::RemoveAgent { agent_id } => self.cmd_remove_agent(agent_id).await,
            KernelCommand::RunTask {
                agent_name,
                prompt,
                autonomous,
                no_checkpoint,
                thinking_level,
            } => {
                self.cmd_run_task(
                    agent_name,
                    prompt,
                    autonomous,
                    no_checkpoint,
                    thinking_level,
                )
                .await
            }
            KernelCommand::ListTasks => self.cmd_list_tasks().await,
            KernelCommand::SetSecret {
                name,
                value,
                scope,
                scope_raw,
            } => {
                // Intentionally calls cmd_set_secret directly (not api_set_secret):
                // the bus command carries scope_raw (raw CLI string scope) which
                // api_set_secret hard-codes to None.
                self.cmd_set_secret(name, value, scope, scope_raw).await
            }
            KernelCommand::ListSecrets => self.cmd_list_secrets().await,
            KernelCommand::RotateSecret { name, new_value } => {
                self.cmd_rotate_secret(name, new_value).await
            }
            KernelCommand::RevokeSecret { name } => match self.api_revoke_secret(name).await {
                Ok(()) => agentos_bus::KernelResponse::Success { data: None },
                Err(msg) => agentos_bus::KernelResponse::Error { message: msg },
            },
            KernelCommand::GetTaskLogs { task_id } => self.cmd_get_task_logs(task_id).await,
            KernelCommand::CancelTask { task_id } => self.cmd_cancel_task(task_id).await,
            KernelCommand::TaskGetTrace { task_id } => self.cmd_get_task_trace(task_id).await,
            KernelCommand::TaskListTraces { agent_id, limit } => {
                self.cmd_list_task_traces(agent_id, limit).await
            }
            KernelCommand::ListTools => self.cmd_list_tools().await,
            KernelCommand::InstallTool { manifest_path } => {
                match self.api_install_tool(manifest_path).await {
                    Ok(()) => agentos_bus::KernelResponse::Success { data: None },
                    Err(msg) => agentos_bus::KernelResponse::Error { message: msg },
                }
            }
            KernelCommand::ToolLoad { manifest_path } => self.cmd_tool_load(manifest_path).await,
            KernelCommand::RemoveTool { tool_name } => {
                match self.api_remove_tool(tool_name).await {
                    Ok(()) => agentos_bus::KernelResponse::Success { data: None },
                    Err(msg) => agentos_bus::KernelResponse::Error { message: msg },
                }
            }
            KernelCommand::GrantPermission {
                agent_name,
                permission,
            } => match self.api_grant_permission(agent_name, permission).await {
                Ok(()) => agentos_bus::KernelResponse::Success { data: None },
                Err(msg) => agentos_bus::KernelResponse::Error { message: msg },
            },
            KernelCommand::RevokePermission {
                agent_name,
                permission,
            } => match self.api_revoke_permission(agent_name, permission).await {
                Ok(()) => agentos_bus::KernelResponse::Success { data: None },
                Err(msg) => agentos_bus::KernelResponse::Error { message: msg },
            },
            KernelCommand::ShowPermissions { agent_name } => {
                self.cmd_show_permissions(agent_name).await
            }
            KernelCommand::CreateRole {
                role_name,
                description,
            } => self.cmd_create_role(role_name, description).await,
            KernelCommand::DeleteRole { role_name } => self.cmd_delete_role(role_name).await,
            KernelCommand::ListRoles => self.cmd_list_roles().await,
            KernelCommand::RoleGrant {
                role_name,
                permission,
            } => self.cmd_role_grant(role_name, permission).await,
            KernelCommand::RoleRevoke {
                role_name,
                permission,
            } => self.cmd_role_revoke(role_name, permission).await,
            KernelCommand::AssignRole {
                agent_name,
                role_name,
            } => self.cmd_assign_role(agent_name, role_name).await,
            KernelCommand::RemoveRole {
                agent_name,
                role_name,
            } => self.cmd_remove_role(agent_name, role_name).await,
            KernelCommand::GetStatus => self.cmd_get_status().await,
            KernelCommand::GetAuditLogs { limit } => self.cmd_get_audit_logs(limit).await,
            KernelCommand::VerifyAuditChain { from_seq } => {
                match self.audit.verify_chain(from_seq) {
                    Ok(verification) => agentos_bus::KernelResponse::Success {
                        data: Some(serde_json::to_value(verification).unwrap_or_default()),
                    },
                    Err(e) => agentos_bus::KernelResponse::Error {
                        message: e.to_string(),
                    },
                }
            }
            KernelCommand::SendAgentMessage {
                from_name,
                to_name,
                content,
            } => {
                self.cmd_send_agent_message(from_name, to_name, content)
                    .await
            }
            KernelCommand::ListAgentMessages { agent_name, limit } => {
                self.cmd_list_agent_messages(agent_name, limit).await
            }
            KernelCommand::CreateAgentGroup {
                group_name,
                members,
            } => self.cmd_create_agent_group(group_name, members).await,
            KernelCommand::BroadcastToGroup {
                from_name,
                group_name,
                content,
            } => {
                self.cmd_broadcast_to_group(from_name, group_name, content)
                    .await
            }
            KernelCommand::CreatePermProfile {
                name,
                description,
                permissions,
            } => {
                self.cmd_create_perm_profile(name, description, permissions)
                    .await
            }
            KernelCommand::DeletePermProfile { name } => self.cmd_delete_perm_profile(name).await,
            KernelCommand::ListPermProfiles => self.cmd_list_perm_profiles().await,
            KernelCommand::AssignPermProfile {
                agent_name,
                profile_name,
            } => self.cmd_assign_perm_profile(agent_name, profile_name).await,
            KernelCommand::GrantPermissionTimed {
                agent_name,
                permission,
                expires_secs,
            } => {
                // TODO: Add api_grant_permission_timed wrapper and route through it,
                // mirroring the Phase 7 migration done for GrantPermission.
                self.cmd_grant_permission_timed(agent_name, permission, expires_secs)
                    .await
            }

            // agentd
            KernelCommand::CreateSchedule {
                name,
                cron,
                agent_name,
                task,
                permissions,
            } => {
                self.cmd_create_schedule(name, cron, agent_name, task, permissions)
                    .await
            }
            KernelCommand::ListSchedules => self.cmd_list_schedules().await,
            KernelCommand::PauseSchedule { name } => self.cmd_pause_schedule(name).await,
            KernelCommand::ResumeSchedule { name } => self.cmd_resume_schedule(name).await,
            KernelCommand::DeleteSchedule { name } => self.cmd_delete_schedule(name).await,
            KernelCommand::RunBackground {
                name,
                agent_name,
                task,
                detach,
            } => {
                self.cmd_run_background(name, agent_name, task, detach)
                    .await
            }
            KernelCommand::ListBackground => self.cmd_list_background().await,
            KernelCommand::GetBackgroundLogs { name, follow } => {
                self.cmd_get_background_logs(name, follow).await
            }
            KernelCommand::KillBackground { name } => self.cmd_kill_background(name).await,

            // Cost management
            KernelCommand::GetCostReport { agent_name } => {
                self.cmd_get_cost_report(agent_name).await
            }
            KernelCommand::GetRetrievalMetrics => self.cmd_get_retrieval_metrics().await,

            // Escalation management
            KernelCommand::ListEscalations { pending_only } => {
                self.cmd_list_escalations(pending_only).await
            }
            KernelCommand::GetEscalation { id } => self.cmd_get_escalation(id).await,
            KernelCommand::ResolveEscalation { id, decision } => {
                self.cmd_resolve_escalation(id, decision).await
            }

            // Pipeline management
            KernelCommand::InstallPipeline { yaml } => self.cmd_install_pipeline(yaml).await,
            KernelCommand::RunPipeline {
                name,
                input,
                detach,
                agent_name,
            } => self.cmd_run_pipeline(name, input, detach, agent_name).await,
            KernelCommand::PipelineStatus { name: _, run_id } => {
                self.cmd_pipeline_status(run_id).await
            }
            KernelCommand::PipelineList => self.cmd_pipeline_list().await,
            KernelCommand::PipelineLogs {
                name: _,
                run_id,
                step_id,
            } => self.cmd_pipeline_logs(run_id, step_id).await,
            KernelCommand::RemovePipeline { name } => self.cmd_remove_pipeline(name).await,

            // Resource arbitration
            KernelCommand::ListResourceLocks => {
                let data = self.cmd_resource_list().await;
                let locks = data
                    .get("locks")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                agentos_bus::KernelResponse::ResourceLockList(locks)
            }
            KernelCommand::ReleaseResourceLock {
                resource_id,
                agent_name,
            } => {
                let data = self.cmd_resource_release(&resource_id, &agent_name).await;
                agentos_bus::KernelResponse::Success { data: Some(data) }
            }
            KernelCommand::ReleaseAllResourceLocks { agent_name } => {
                let data = self.cmd_resource_release_all(&agent_name).await;
                agentos_bus::KernelResponse::Success { data: Some(data) }
            }

            KernelCommand::ListSnapshots { task_id } => self.cmd_list_snapshots(task_id).await,
            KernelCommand::RollbackTask {
                task_id,
                snapshot_ref,
            } => self.cmd_rollback_task(task_id, snapshot_ref).await,

            // Checkpoint recovery
            KernelCommand::ResumeTask { task_id } => self.cmd_resume_task(task_id).await,
            KernelCommand::ListCheckpoints => self.cmd_list_checkpoints().await,

            // Event system
            KernelCommand::EventSubscribe {
                agent_name,
                event_filter,
                payload_filter,
                throttle,
                priority,
            } => {
                self.cmd_event_subscribe(
                    agent_name,
                    event_filter,
                    payload_filter,
                    throttle,
                    priority,
                )
                .await
            }
            KernelCommand::EventUnsubscribe { subscription_id } => {
                self.cmd_event_unsubscribe(subscription_id).await
            }
            KernelCommand::EventListSubscriptions { agent_name } => {
                self.cmd_event_list_subscriptions(agent_name).await
            }
            KernelCommand::EventGetSubscription { subscription_id } => {
                self.cmd_event_get_subscription(subscription_id).await
            }
            KernelCommand::EventEnableSubscription { subscription_id } => {
                self.cmd_event_enable_subscription(subscription_id).await
            }
            KernelCommand::EventDisableSubscription { subscription_id } => {
                self.cmd_event_disable_subscription(subscription_id).await
            }
            KernelCommand::EventHistory { last } => self.cmd_event_history(last).await,

            // Vault lockdown
            KernelCommand::VaultLockdown => self.cmd_vault_lockdown().await,

            // Identity management
            KernelCommand::IdentityShow { agent_name } => self.cmd_identity_show(agent_name).await,
            KernelCommand::IdentityRevoke { agent_name } => {
                self.cmd_identity_revoke(agent_name).await
            }

            // Audit export
            KernelCommand::ExportAuditChain { limit } => self.cmd_export_audit_chain(limit).await,

            // Resource contention
            KernelCommand::ResourceContention => self.cmd_resource_contention().await,

            // Hardware Abstraction Layer
            KernelCommand::HalListDevices => {
                let devices = self.cmd_hal_list_devices().await;
                agentos_bus::KernelResponse::HalDeviceList(devices)
            }
            KernelCommand::HalRegisterDevice {
                device_id,
                device_type,
            } => {
                let data = self.cmd_hal_register_device(&device_id, &device_type).await;
                agentos_bus::KernelResponse::Success { data: Some(data) }
            }
            KernelCommand::HalApproveDevice {
                device_id,
                agent_name,
            } => {
                let data = self.cmd_hal_approve_device(&device_id, &agent_name).await;
                agentos_bus::KernelResponse::Success { data: Some(data) }
            }
            KernelCommand::HalDenyDevice { device_id } => {
                let data = self.cmd_hal_deny_device(&device_id).await;
                agentos_bus::KernelResponse::Success { data: Some(data) }
            }
            KernelCommand::HalRevokeDevice {
                device_id,
                agent_name,
            } => {
                let data = self.cmd_hal_revoke_device(&device_id, &agent_name).await;
                agentos_bus::KernelResponse::Success { data: Some(data) }
            }

            KernelCommand::SetLogLevel { level } => self.cmd_set_log_level(level).await,

            // Notification system (UNIS Phase 1)
            KernelCommand::SendUserNotification {
                subject,
                body,
                priority,
                kind,
                trace_id,
                from_agent,
            } => {
                self.cmd_send_user_notification(subject, body, priority, kind, trace_id, from_agent)
                    .await
            }
            KernelCommand::ListNotifications { unread_only, limit } => {
                self.cmd_list_notifications(unread_only, limit).await
            }
            KernelCommand::GetNotification { notification_id } => {
                self.cmd_get_notification(notification_id).await
            }
            KernelCommand::MarkNotificationRead { notification_id } => {
                self.cmd_mark_notification_read(notification_id).await
            }
            KernelCommand::RespondToNotification {
                notification_id,
                response_text,
                channel,
            } => {
                self.cmd_respond_to_notification(notification_id, response_text, channel)
                    .await
            }

            KernelCommand::ConnectChannel {
                kind,
                external_id,
                display_name,
                credential_key,
                reply_topic,
                server_url,
                webhook_url,
                active_agent_name,
            } => {
                self.cmd_connect_channel(
                    kind,
                    external_id.unwrap_or_default(),
                    display_name,
                    credential_key,
                    reply_topic,
                    server_url,
                    webhook_url,
                    active_agent_name,
                )
                .await
            }
            KernelCommand::SetChannelActiveAgent {
                channel_id,
                agent_name,
            } => {
                self.cmd_set_channel_active_agent(channel_id, agent_name)
                    .await
            }
            KernelCommand::DisconnectChannel { channel_id } => {
                self.cmd_disconnect_channel(channel_id).await
            }
            KernelCommand::ListChannels => self.cmd_list_channels().await,
            KernelCommand::TestChannel { channel_id } => self.cmd_test_channel(channel_id).await,
            KernelCommand::ListPlugins => self.cmd_list_plugins().await,
            KernelCommand::EnablePlugin { plugin_id } => self.cmd_enable_plugin(plugin_id).await,
            KernelCommand::DisablePlugin { plugin_id } => self.cmd_disable_plugin(plugin_id).await,
            KernelCommand::McpStatus => self.cmd_mcp_status().await,
            KernelCommand::McpAttach {
                name,
                command,
                args,
                url,
                auth_token,
                oauth_connector_id,
                timeout_secs,
                env,
            } => {
                self.cmd_mcp_attach(
                    name,
                    command,
                    args,
                    url,
                    auth_token,
                    oauth_connector_id,
                    timeout_secs,
                    env,
                )
                .await
            }
            KernelCommand::McpDetach { name } => self.cmd_mcp_detach(name).await,
            KernelCommand::McpOAuthStore {
                connector_id,
                provider,
                access_token,
                refresh_token,
                token_endpoint,
                client_id,
                client_secret,
                scopes,
                expires_in_secs,
            } => {
                self.cmd_mcp_oauth_store(
                    connector_id,
                    provider,
                    access_token,
                    refresh_token,
                    token_endpoint,
                    client_id,
                    client_secret,
                    scopes,
                    expires_in_secs,
                )
                .await
            }

            // Context memory
            KernelCommand::ContextMemoryRead { agent_id } => {
                self.cmd_context_memory_read(agent_id).await
            }
            KernelCommand::ContextMemoryUpdate {
                agent_id,
                content,
                reason,
            } => {
                self.cmd_context_memory_update(agent_id, content, reason)
                    .await
            }
            KernelCommand::ContextMemoryHistory { agent_id, limit } => {
                self.cmd_context_memory_history(agent_id, limit).await
            }
            KernelCommand::ContextMemoryRollback { agent_id, version } => {
                self.cmd_context_memory_rollback(agent_id, version).await
            }
            KernelCommand::ContextMemoryClear { agent_id } => {
                self.cmd_context_memory_clear(agent_id).await
            }
            KernelCommand::ContextMemorySet { agent_id, content } => {
                self.cmd_context_memory_set(agent_id, content).await
            }

            KernelCommand::ScratchListPages { agent_id } => {
                self.cmd_scratch_list_pages(agent_id).await
            }
            KernelCommand::ScratchReadPage { agent_id, title } => {
                self.cmd_scratch_read_page(agent_id, title).await
            }
            KernelCommand::ScratchDeletePage { agent_id, title } => {
                self.cmd_scratch_delete_page(agent_id, title).await
            }
            KernelCommand::ScratchGraphPage {
                agent_id,
                title,
                depth,
            } => self.cmd_scratch_graph_page(agent_id, title, depth).await,

            KernelCommand::Shutdown => {
                tracing::info!("Shutdown command received, initiating graceful shutdown");
                self.audit_shutdown("shutdown_command", agentos_audit::AuditSeverity::Info);
                self.cancellation_token.cancel();
                agentos_bus::KernelResponse::Success {
                    data: Some(serde_json::json!({ "status": "shutting_down" })),
                }
            }

            // Skills management
            KernelCommand::SkillInstall { path } => self.cmd_skill_install(path).await,
            KernelCommand::SkillRemove { name } => self.cmd_skill_remove(name).await,
            KernelCommand::SkillList => self.cmd_skill_list().await,
            KernelCommand::SkillRun { name, input } => self.cmd_skill_run(name, input).await,
            KernelCommand::SkillStatus { name } => self.cmd_skill_status(name).await,

            // Provider catalog
            KernelCommand::ListProviders => self.cmd_list_providers().await,
            KernelCommand::SetProviderUrl { name, url } => {
                self.cmd_set_provider_url(name, url).await
            }
            KernelCommand::AddProvider { entry_json } => self.cmd_add_provider(entry_json).await,
            KernelCommand::RemoveProvider { name } => self.cmd_remove_provider(name).await,
            KernelCommand::ProbeProviderModels { name } => {
                self.cmd_probe_provider_models(name).await
            }

            // Sub-agent coordination
            KernelCommand::SpawnSubAgent {
                parent_task_id,
                agent_name,
                prompt,
                requested_permissions,
                context_slice,
                handoff_mode,
                tool_categories,
            } => {
                self.cmd_spawn_sub_agent(
                    parent_task_id,
                    &agent_name,
                    &prompt,
                    &requested_permissions,
                    context_slice,
                    handoff_mode,
                    tool_categories,
                )
                .await
            }
            KernelCommand::AwaitSubAgents {
                parent_task_id,
                child_task_ids,
            } => {
                self.cmd_await_sub_agents(parent_task_id, &child_task_ids)
                    .await
            }
            KernelCommand::RunTeam { config } => self.cmd_run_team(&config).await,
            KernelCommand::TeamStatus { team_task_id } => {
                // Return the task summary for the coordinator task.
                self.cmd_get_task_logs(team_task_id).await
            }

            // Webhook endpoint management
            KernelCommand::CreateWebhookEndpoint {
                agent_name,
                provider,
                debounce_seconds,
            } => {
                self.cmd_create_webhook_endpoint(&agent_name, &provider, debounce_seconds)
                    .await
            }
            KernelCommand::ListWebhookEndpoints { agent_name } => {
                self.cmd_list_webhook_endpoints(agent_name.as_deref()).await
            }
            KernelCommand::DeleteWebhookEndpoint { endpoint_id } => {
                self.cmd_delete_webhook_endpoint(&endpoint_id).await
            }

            // Container runtime management
            KernelCommand::ContainerCreate {
                agent_name,
                image,
                memory_mb,
                cpu,
                network,
                ttl_seconds,
            } => {
                self.cmd_container_create(agent_name, image, memory_mb, cpu, network, ttl_seconds)
                    .await
            }
            KernelCommand::ContainerExec {
                agent_name,
                container_id,
                command,
                timeout_ms,
            } => {
                self.cmd_container_exec(agent_name, container_id, command, timeout_ms)
                    .await
            }
            KernelCommand::ContainerLogs {
                agent_name,
                container_id,
                tail,
            } => {
                self.cmd_container_logs(agent_name, container_id, tail)
                    .await
            }
            KernelCommand::ContainerDestroy {
                agent_name,
                container_id,
            } => self.cmd_container_destroy(agent_name, container_id).await,
            KernelCommand::ContainerList { agent_name } => {
                self.cmd_container_list(agent_name).await
            }
        }
    }

    /// The agentd scheduler loop — checks for due scheduled jobs and fires them.
    pub(crate) async fn agentd_loop(&self) {
        loop {
            tokio::select! {
                _ = self.cancellation_token.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }

            let due_jobs = self.schedule_manager.check_due_jobs().await;
            for job in due_jobs {
                use agentos_types::schedule::{
                    OnceJobAction, RunParentKind, RunState, ScheduledRun,
                };
                use agentos_types::{
                    NotificationID, NotificationPriority, NotificationSource, RunID, UserMessage,
                    UserMessageKind,
                };

                let action_tag = job.action.tag();
                tracing::info!(job_name = %job.name, action = %action_tag, "Firing scheduled job");

                let trace_id = agentos_types::TraceID::new();
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ScheduledJobFired,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "job_name": job.name,
                        "action": action_tag,
                        "once": false,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                // Open a run record up-front so `get-schedule-runs` returns
                // history even for synchronous fires. Updated below per
                // action outcome.
                let run_id = RunID::new();
                let run_started = chrono::Utc::now();
                let mk_run = |state: RunState,
                              task_id: Option<agentos_types::TaskID>,
                              error: Option<String>,
                              completed_at: Option<chrono::DateTime<chrono::Utc>>|
                 -> ScheduledRun {
                    ScheduledRun {
                        run_id,
                        parent_kind: RunParentKind::Schedule,
                        parent_id: job.id,
                        parent_name: Some(job.name.clone()),
                        creator_agent_id: job.creator_agent_id,
                        task_id,
                        state,
                        started_at: run_started,
                        completed_at,
                        result: None,
                        error,
                        tool_calls: vec![],
                        delivery: job.delivery.clone(),
                        delivered: false,
                        delivered_at: None,
                        delivery_error: None,
                        delivery_depth: None,
                    }
                };

                match job.action.clone() {
                    OnceJobAction::RunTask { prompt } => {
                        match self
                            .create_background_task(
                                job.name.clone(),
                                job.agent_name.clone(),
                                prompt,
                                true,
                                true,
                            )
                            .await
                        {
                            Ok(task_id) => {
                                // Link the spawned task to the scheduled job so that
                                // complete_task_success can emit ScheduledTaskCompleted.
                                self.background_pool
                                    .set_scheduled_job(&task_id, job.id)
                                    .await;
                                // Race-free: track in-memory FIRST so completion
                                // never races the SQLite upsert.
                                self.schedule_manager
                                    .track_pending_run(task_id, run_id)
                                    .await;
                                if let Some(store) = self.schedule_manager.store() {
                                    let run = mk_run(RunState::Running, Some(task_id), None, None);
                                    if let Err(e) = store.upsert_run(run).await {
                                        tracing::warn!(error = %e, "Failed to persist running ScheduledRun");
                                    }
                                }
                            }
                            Err(agentos_types::AgentOSError::AgentNotFound(_)) => {
                                tracing::warn!(
                                    job_name = %job.name,
                                    agent_name = %job.agent_name,
                                    "Scheduled job target agent not found"
                                );
                                self.schedule_manager
                                    .emit_task_missed(&job, "target agent not registered")
                                    .await;
                                if let Some(store) = self.schedule_manager.store() {
                                    let now = chrono::Utc::now();
                                    let run = mk_run(
                                        RunState::Missed,
                                        None,
                                        Some("target agent not registered".into()),
                                        Some(now),
                                    );
                                    let _ = store.upsert_run(run).await;
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    job_name = %job.name,
                                    error = %e,
                                    "Scheduled job failed to launch"
                                );
                                self.schedule_manager
                                    .emit_task_failed(&job, &e.to_string())
                                    .await;
                                if let Some(store) = self.schedule_manager.store() {
                                    let now = chrono::Utc::now();
                                    let run = mk_run(
                                        RunState::Failed,
                                        None,
                                        Some(e.to_string()),
                                        Some(now),
                                    );
                                    let _ = store.upsert_run(run).await;
                                }
                            }
                        }
                    }
                    OnceJobAction::NotifyUser {
                        subject,
                        body,
                        priority: priority_str,
                    } => {
                        let priority = match priority_str.as_str() {
                            "warning" => NotificationPriority::Warning,
                            "urgent" => NotificationPriority::Urgent,
                            "critical" => NotificationPriority::Critical,
                            _ => NotificationPriority::Info,
                        };
                        let msg = UserMessage {
                            id: NotificationID::new(),
                            from: NotificationSource::Kernel,
                            task_id: None,
                            trace_id,
                            kind: UserMessageKind::Notification,
                            priority,
                            subject,
                            body,
                            interaction: None,
                            delivery_status: std::collections::HashMap::new(),
                            response: None,
                            created_at: chrono::Utc::now(),
                            expires_at: None,
                            read: false,
                            thread_id: None,
                            reply_to_external_id: None,
                        };
                        let now = chrono::Utc::now();
                        if let Err(e) = self.notification_router.deliver(msg).await {
                            tracing::warn!(job_name = %job.name, error = %e, "Cron notification delivery failed");
                            self.schedule_manager
                                .emit_task_failed(&job, &e.to_string())
                                .await;
                            if let Some(store) = self.schedule_manager.store() {
                                let run =
                                    mk_run(RunState::Failed, None, Some(e.to_string()), Some(now));
                                if store.upsert_run(run).await.is_ok() {
                                    self.dispatch_scheduled_delivery(run_id).await;
                                }
                            }
                        } else if let Some(store) = self.schedule_manager.store() {
                            let run = mk_run(RunState::Complete, None, None, Some(now));
                            if store.upsert_run(run).await.is_ok() {
                                self.dispatch_scheduled_delivery(run_id).await;
                            }
                        }
                    }
                    OnceJobAction::RunTool { tool, args } => {
                        self.fire_scheduled_tool(job.agent_name.clone(), tool, args, trace_id)
                            .await;
                        // Tool fires synchronously in `fire_scheduled_tool`; record
                        // a Complete run + dispatch its DeliveryMode.
                        if let Some(store) = self.schedule_manager.store() {
                            let run =
                                mk_run(RunState::Complete, None, None, Some(chrono::Utc::now()));
                            if store.upsert_run(run).await.is_ok() {
                                self.dispatch_scheduled_delivery(run_id).await;
                            }
                        }
                    }
                }
            }

            // Fire due in-memory timers.
            let due_timers = self.schedule_manager.check_due_timers().await;
            for timer in due_timers {
                tracing::info!(timer_name = %timer.name, "Firing timer");
                self.fire_timer(timer).await;
            }

            // Fire due once-jobs.
            let due_once = self.schedule_manager.check_due_once_jobs().await;
            for job in due_once {
                use agentos_types::schedule::{
                    OnceJobAction, RunParentKind, RunState, ScheduledRun,
                };
                use agentos_types::{
                    NotificationID, NotificationPriority, NotificationSource, RunID, UserMessage,
                    UserMessageKind,
                };
                let trace_id = agentos_types::TraceID::new();
                tracing::info!(job_name = %job.name, action = %job.action.tag(), "Firing once-job");
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ScheduledJobFired,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "job_name": job.name,
                        "once": true,
                        "action": job.action.tag(),
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                let run_id = RunID::new();
                let run_started = chrono::Utc::now();
                let job_creator = job.creator_agent_id;
                let job_id = job.id;
                let job_name = job.name.clone();
                let job_delivery = job.delivery.clone();
                let mk_once_run = |state: RunState,
                                   task_id: Option<agentos_types::TaskID>,
                                   error: Option<String>,
                                   completed_at: Option<chrono::DateTime<chrono::Utc>>|
                 -> ScheduledRun {
                    ScheduledRun {
                        run_id,
                        parent_kind: RunParentKind::Once,
                        parent_id: job_id,
                        parent_name: Some(job_name.clone()),
                        creator_agent_id: job_creator,
                        task_id,
                        state,
                        started_at: run_started,
                        completed_at,
                        result: None,
                        error,
                        tool_calls: vec![],
                        delivery: job_delivery.clone(),
                        delivered: false,
                        delivered_at: None,
                        delivery_error: None,
                        delivery_depth: None,
                    }
                };

                match job.action.clone() {
                    OnceJobAction::RunTask { prompt } => {
                        match self
                            .create_background_task(
                                job.name.clone(),
                                job.agent_name.clone(),
                                prompt,
                                false,
                                true,
                            )
                            .await
                        {
                            Ok(task_id) => {
                                self.background_pool
                                    .set_scheduled_job(&task_id, job.id)
                                    .await;
                                self.schedule_manager
                                    .track_pending_run(task_id, run_id)
                                    .await;
                                if let Some(store) = self.schedule_manager.store() {
                                    let run =
                                        mk_once_run(RunState::Running, Some(task_id), None, None);
                                    if let Err(e) = store.upsert_run(run).await {
                                        tracing::warn!(error = %e, "Failed to persist Running once-job run");
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(job_name = %job.name, error = %e, "Once-job task launch failed");
                                if let Some(store) = self.schedule_manager.store() {
                                    let now = chrono::Utc::now();
                                    let run = mk_once_run(
                                        RunState::Failed,
                                        None,
                                        Some(e.to_string()),
                                        Some(now),
                                    );
                                    if store.upsert_run(run).await.is_ok() {
                                        self.dispatch_scheduled_delivery(run_id).await;
                                    }
                                }
                            }
                        }
                    }
                    OnceJobAction::NotifyUser {
                        subject,
                        body,
                        priority: priority_str,
                    } => {
                        let priority = match priority_str.as_str() {
                            "warning" => NotificationPriority::Warning,
                            "urgent" => NotificationPriority::Urgent,
                            "critical" => NotificationPriority::Critical,
                            _ => NotificationPriority::Info,
                        };
                        let msg = UserMessage {
                            id: NotificationID::new(),
                            from: NotificationSource::Kernel,
                            task_id: None,
                            trace_id,
                            kind: UserMessageKind::Notification,
                            priority,
                            subject,
                            body,
                            interaction: None,
                            delivery_status: std::collections::HashMap::new(),
                            response: None,
                            created_at: chrono::Utc::now(),
                            expires_at: None,
                            read: false,
                            thread_id: None,
                            reply_to_external_id: None,
                        };
                        let now = chrono::Utc::now();
                        if let Err(e) = self.notification_router.deliver(msg).await {
                            tracing::warn!(job_name = %job.name, error = %e, "Once-job notification delivery failed");
                            if let Some(store) = self.schedule_manager.store() {
                                let run = mk_once_run(
                                    RunState::Failed,
                                    None,
                                    Some(e.to_string()),
                                    Some(now),
                                );
                                if store.upsert_run(run).await.is_ok() {
                                    self.dispatch_scheduled_delivery(run_id).await;
                                }
                            }
                        } else if let Some(store) = self.schedule_manager.store() {
                            let run = mk_once_run(RunState::Complete, None, None, Some(now));
                            if store.upsert_run(run).await.is_ok() {
                                self.dispatch_scheduled_delivery(run_id).await;
                            }
                        }
                    }
                    OnceJobAction::RunTool { tool, args } => {
                        self.fire_scheduled_tool(job.agent_name.clone(), tool, args, trace_id)
                            .await;
                        if let Some(store) = self.schedule_manager.store() {
                            let run = mk_once_run(
                                RunState::Complete,
                                None,
                                None,
                                Some(chrono::Utc::now()),
                            );
                            if store.upsert_run(run).await.is_ok() {
                                self.dispatch_scheduled_delivery(run_id).await;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Dispatch a fired timer action: deliver notification and/or launch a background task.
    async fn fire_timer(&self, timer: agentos_types::schedule::TimerEntry) {
        use agentos_types::schedule::{RunParentKind, RunState, ScheduledRun, TimerAction};
        use agentos_types::{
            NotificationID, NotificationPriority, NotificationSource, RunID, TraceID, UserMessage,
            UserMessageKind,
        };
        use std::collections::HashMap;

        let trace_id = TraceID::new();
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::TimerFired,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "timer_name": timer.name,
                "timer_id": timer.id.to_string(),
                "agent_name": timer.agent_name,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        // Open a ScheduledRun for this timer fire so delivery + audit have a
        // backing record (Timers are evicted from memory immediately after
        // fire; the run is the only durable trace).
        let run_id = RunID::new();
        let run_started = chrono::Utc::now();
        let timer_id = timer.id;
        let timer_name = timer.name.clone();
        let timer_creator = timer.creator_agent_id;
        let timer_delivery = timer.delivery.clone();
        let mk_timer_run = |state: RunState,
                            task_id: Option<agentos_types::TaskID>,
                            error: Option<String>,
                            completed_at: Option<chrono::DateTime<chrono::Utc>>|
         -> ScheduledRun {
            ScheduledRun {
                run_id,
                parent_kind: RunParentKind::Timer,
                parent_id: timer_id,
                parent_name: Some(timer_name.clone()),
                creator_agent_id: timer_creator,
                task_id,
                state,
                started_at: run_started,
                completed_at,
                result: None,
                error,
                tool_calls: vec![],
                delivery: timer_delivery.clone(),
                delivered: false,
                delivered_at: None,
                delivery_error: None,
                delivery_depth: None,
            }
        };

        let deliver_notification = |subject: String, body: String, priority_str: String| {
            let priority = match priority_str.as_str() {
                "warning" => NotificationPriority::Warning,
                "urgent" => NotificationPriority::Urgent,
                "critical" => NotificationPriority::Critical,
                _ => NotificationPriority::Info,
            };
            UserMessage {
                id: NotificationID::new(),
                from: NotificationSource::Kernel,
                task_id: None,
                trace_id: TraceID::new(),
                kind: UserMessageKind::Notification,
                priority,
                subject,
                body,
                interaction: None,
                delivery_status: HashMap::new(),
                response: None,
                created_at: chrono::Utc::now(),
                expires_at: None,
                read: false,
                thread_id: None,
                reply_to_external_id: None,
            }
        };

        match timer.action {
            TimerAction::NotifyUser {
                subject,
                body,
                priority,
            } => {
                let msg = deliver_notification(subject, body, priority);
                let now = chrono::Utc::now();
                if let Err(e) = self.notification_router.deliver(msg).await {
                    tracing::warn!(timer_name = %timer.name, error = %e, "Timer notification delivery failed");
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id,
                        event_type: agentos_audit::AuditEventType::TimerActionFailed,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "timer_name": timer.name, "action": "notify_user", "error": e.to_string() }),
                        severity: agentos_audit::AuditSeverity::Warn,
                        reversible: false,
                        rollback_ref: None,
                    });
                    if let Some(store) = self.schedule_manager.store() {
                        let run =
                            mk_timer_run(RunState::Failed, None, Some(e.to_string()), Some(now));
                        if store.upsert_run(run).await.is_ok() {
                            self.dispatch_scheduled_delivery(run_id).await;
                        }
                    }
                } else if let Some(store) = self.schedule_manager.store() {
                    let run = mk_timer_run(RunState::Complete, None, None, Some(now));
                    if store.upsert_run(run).await.is_ok() {
                        self.dispatch_scheduled_delivery(run_id).await;
                    }
                }
            }
            TimerAction::RunTask { prompt } => {
                match self
                    .create_background_task(
                        timer.name.clone(),
                        timer.agent_name.clone(),
                        prompt,
                        false,
                        true,
                    )
                    .await
                {
                    Ok(task_id) => {
                        self.schedule_manager
                            .track_pending_run(task_id, run_id)
                            .await;
                        if let Some(store) = self.schedule_manager.store() {
                            let run = mk_timer_run(RunState::Running, Some(task_id), None, None);
                            if let Err(e) = store.upsert_run(run).await {
                                tracing::warn!(error = %e, "Failed to persist Running timer run");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(timer_name = %timer.name, error = %e, "Timer task launch failed");
                        self.audit_log(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id,
                            event_type: agentos_audit::AuditEventType::TimerActionFailed,
                            agent_id: None,
                            task_id: None,
                            tool_id: None,
                            details: serde_json::json!({ "timer_name": timer.name, "action": "run_task", "error": e.to_string() }),
                            severity: agentos_audit::AuditSeverity::Warn,
                            reversible: false,
                            rollback_ref: None,
                        });
                        if let Some(store) = self.schedule_manager.store() {
                            let now = chrono::Utc::now();
                            let run = mk_timer_run(
                                RunState::Failed,
                                None,
                                Some(e.to_string()),
                                Some(now),
                            );
                            if store.upsert_run(run).await.is_ok() {
                                self.dispatch_scheduled_delivery(run_id).await;
                            }
                        }
                    }
                }
            }
            TimerAction::RunTaskAndNotify {
                prompt,
                subject,
                body,
                priority,
            } => {
                let msg = deliver_notification(subject, body, priority);
                if let Err(e) = self.notification_router.deliver(msg).await {
                    tracing::warn!(timer_name = %timer.name, error = %e, "Timer notification delivery failed");
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id,
                        event_type: agentos_audit::AuditEventType::TimerActionFailed,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "timer_name": timer.name, "action": "run_task_and_notify/notify", "error": e.to_string() }),
                        severity: agentos_audit::AuditSeverity::Warn,
                        reversible: false,
                        rollback_ref: None,
                    });
                }
                if let Err(e) = self
                    .create_background_task(
                        timer.name.clone(),
                        timer.agent_name.clone(),
                        prompt,
                        false,
                        true,
                    )
                    .await
                {
                    tracing::warn!(timer_name = %timer.name, error = %e, "Timer task launch failed");
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id,
                        event_type: agentos_audit::AuditEventType::TimerActionFailed,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "timer_name": timer.name, "action": "run_task_and_notify/task", "error": e.to_string() }),
                        severity: agentos_audit::AuditSeverity::Warn,
                        reversible: false,
                        rollback_ref: None,
                    });
                }
            }
            TimerAction::RunTool { tool, args } => {
                self.fire_scheduled_tool(timer.agent_name.clone(), tool, args, trace_id)
                    .await;
                if let Some(store) = self.schedule_manager.store() {
                    let run =
                        mk_timer_run(RunState::Complete, None, None, Some(chrono::Utc::now()));
                    if store.upsert_run(run).await.is_ok() {
                        self.dispatch_scheduled_delivery(run_id).await;
                    }
                }
            }
        }
    }

    /// Synthesize a `ToolExecutionContext` and run a single tool by name on
    /// behalf of the scheduling agent. Returns the tool result or an error.
    async fn execute_scheduled_tool(
        &self,
        agent_name: String,
        tool_name: String,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, agentos_types::AgentOSError> {
        use agentos_tools::traits::ToolExecutionContext;
        use agentos_types::{AgentOSError, TaskID, TraceID};

        if crate::schedule_action_policy::args_exceed_size_cap(&args) {
            return Err(AgentOSError::SchemaValidation(format!(
                "tool_args exceeds {} byte cap",
                crate::schedule_action_policy::MAX_TOOL_ARGS_BYTES
            )));
        }

        let agent = {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_name(&agent_name)
                .ok_or_else(|| AgentOSError::AgentNotFound(agent_name.clone()))?
                .clone()
        };

        let permissions = {
            let registry = self.agent_registry.read().await;
            registry.compute_effective_permissions(&agent.id)
        };

        let exec_ctx = ToolExecutionContext {
            data_dir: self.data_dir.clone(),
            task_id: TaskID::new(),
            agent_id: agent.id,
            trace_id: TraceID::new(),
            permissions,
            vault: None,
            hal: Some(self.hal.clone()),
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: self.workspace_paths.clone(),
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: self.cancellation_token.child_token(),
            tool_categories: None,
        };

        self.tool_runner.execute(&tool_name, args, exec_ctx).await
    }

    /// Fire a scheduled `RunTool` action: invoke the tool with a synthetic
    /// per-fire capability scoped to the scheduling agent. No LLM in the loop.
    pub(crate) async fn fire_scheduled_tool(
        &self,
        agent_name: String,
        tool_name: String,
        args: serde_json::Value,
        trace_id: agentos_types::TraceID,
    ) {
        // Defense in depth: re-check the tool name against the schedule denylist
        // even though Phase 4 validates at schedule time.
        if crate::schedule_action_policy::is_tool_blocked_for_schedule(&tool_name) {
            tracing::warn!(
                tool = %tool_name,
                "Scheduled run_tool rejected — tool is on the schedule denylist"
            );
            self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id,
                event_type: agentos_audit::AuditEventType::ScheduledToolFailed,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "tool": tool_name,
                    "agent_name": agent_name,
                    "error": "tool blocked from scheduling",
                }),
                severity: agentos_audit::AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            });
            return;
        }

        match self
            .execute_scheduled_tool(agent_name.clone(), tool_name.clone(), args)
            .await
        {
            Ok(result) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ScheduledToolFired,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "tool": tool_name,
                        "agent_name": agent_name,
                        "result_preview": result.to_string().chars().take(512).collect::<String>(),
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
            }
            Err(e) => {
                tracing::warn!(tool = %tool_name, error = %e, "Scheduled run_tool failed");
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ScheduledToolFailed,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "tool": tool_name,
                        "agent_name": agent_name,
                        "error": e.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculate_restart_delay_is_exponential() {
        let seed = task_name_seed("TestTask");
        let d0 = calculate_restart_delay(0, seed);
        let d1 = calculate_restart_delay(1, seed);
        let d2 = calculate_restart_delay(2, seed);
        assert!(d1.as_millis() > d0.as_millis());
        assert!(d2.as_millis() > d1.as_millis());
    }

    #[test]
    fn calculate_restart_delay_is_capped() {
        let seed = task_name_seed("TestTask");
        let d_max = calculate_restart_delay(100, seed);
        assert!(d_max.as_millis() <= (BACKOFF_MAX_MS + 500) as u128);
    }

    #[test]
    fn calculate_restart_delay_differs_by_task() {
        let d_a = calculate_restart_delay(1, task_name_seed("Acceptor"));
        let d_b = calculate_restart_delay(1, task_name_seed("Consolidation"));
        assert_ne!(
            d_a, d_b,
            "jitter should differ per task to avoid thundering herd"
        );
    }

    #[test]
    fn task_kind_critical_classification() {
        assert!(TaskKind::Acceptor.is_critical());
        assert!(TaskKind::Executor.is_critical());
        assert!(TaskKind::TimeoutChecker.is_critical());
        assert!(TaskKind::EventDispatcher.is_critical());
        assert!(!TaskKind::Consolidation.is_critical());
        assert!(!TaskKind::HealthMonitor.is_critical());
        assert!(!TaskKind::Scheduler.is_critical());
        assert!(!TaskKind::ToolLifecycleListener.is_critical());
    }

    #[test]
    fn circuit_opens_after_max_restarts() {
        let mut state = SubsystemState::new();
        for i in 1..=MAX_RESTARTS {
            state.attempt += 1;
            assert_eq!(state.attempt, i);
            assert!(!state.circuit_open);
        }
        // One more trips the circuit
        state.attempt += 1;
        if state.attempt > MAX_RESTARTS {
            state.circuit_open = true;
        }
        assert!(state.circuit_open);
    }

    #[test]
    fn circuit_recovers_after_window_expires() {
        // Simulate a tripped circuit with a stale window
        let mut state = SubsystemState {
            attempt: MAX_RESTARTS + 1,
            window_start: std::time::Instant::now() - Duration::from_secs(RESTART_WINDOW_SECS + 1),
            circuit_open: true,
        };

        // Apply the window-reset logic from check_restart_with_backoff
        let now = std::time::Instant::now();
        if now.duration_since(state.window_start) > Duration::from_secs(RESTART_WINDOW_SECS) {
            state.attempt = 0;
            state.window_start = now;
            state.circuit_open = false;
        }

        assert!(
            !state.circuit_open,
            "circuit should reset after window expires"
        );
        assert_eq!(state.attempt, 0);
    }
}
