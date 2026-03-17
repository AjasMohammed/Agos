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
        }
    }
}

/// Maximum restarts per task within the restart window before declaring degraded.
const MAX_RESTARTS: u32 = 5;
/// Window in which MAX_RESTARTS is counted (seconds).
const RESTART_WINDOW_SECS: u64 = 60;

impl Kernel {
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
                                        kernel.scheduler.requeue(&waiter_id).await.ok();
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

                                // Sweep expired resource locks (Spec §8)
                                kernel.resource_arbiter.sweep_expired().await;

                                // Sweep expired vault proxy tokens (Spec §3)
                                kernel.vault.sweep_expired_proxy_tokens().await;

                                // Sweep expired escalations — auto-deny (Spec §12)
                                let expired_escalations = kernel.escalation_manager.sweep_expired().await;
                                for (esc_id, task_id, agent_id, blocking, auto_action) in &expired_escalations {
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
                                                                    kernel.scheduler.requeue(&waiter_id).await.ok();
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
                                                            kernel.scheduler.requeue(&waiter_id).await.ok();
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

                                    // Evict terminal background tasks older than 1 hour to
                                    // prevent unbounded pool growth for long-running kernels.
                                    kernel.background_pool.evict_terminal(3600).await;

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

        // Track restart counts per task kind
        let mut restart_counts: std::collections::HashMap<String, (u32, std::time::Instant)> =
            std::collections::HashMap::new();

        // Spawn all 11 core tasks
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
        ];

        for kind in &all_kinds {
            Self::spawn_tracked_task(&mut join_set, &mut task_id_map, *kind, self.clone());
        }

        // Install Prometheus metrics recorder and start health/readiness/metrics HTTP server
        if let Some(prom_handle) = crate::health::install_prometheus_recorder() {
            if let Err(e) = crate::health::start_health_server(self.clone(), prom_handle).await {
                tracing::warn!(error = %e, "Failed to start health server, continuing without it");
            }
        }

        tracing::info!("Kernel supervisor started with {} tasks", all_kinds.len());

        loop {
            let next = tokio::select! {
                _ = self.cancellation_token.cancelled() => {
                    tracing::info!("Kernel shutdown requested, stopping supervisor");
                    join_set.abort_all();
                    break;
                }
                next = join_set.join_next() => next,
            };
            match next {
                Some(Ok(kind)) => {
                    // Task completed normally — unexpected for long-running loops
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

                    if self.check_restart_budget(&mut restart_counts, &kind.to_string()) {
                        Self::spawn_tracked_task(
                            &mut join_set,
                            &mut task_id_map,
                            kind,
                            self.clone(),
                        );
                    } else {
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
                        tracing::error!(task = %kind, "Task exceeded restart budget, kernel degraded");
                        break;
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
                        tracing::error!(task = %task_name, "Kernel task panicked: {:?}", join_error);
                    } else {
                        tracing::error!(task = %task_name, "Kernel task cancelled: {:?}", join_error);
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

                    if self.check_restart_budget(&mut restart_counts, &task_name) {
                        if let Some(kind) = identified_kind {
                            // Targeted restart: only restart the specific crashed task
                            tracing::info!(task = %kind, "Restarting only the crashed task");
                            Self::spawn_tracked_task(
                                &mut join_set,
                                &mut task_id_map,
                                kind,
                                self.clone(),
                            );
                        } else {
                            // Fallback: if we can't identify which task crashed, restart all
                            tracing::warn!(
                                "Could not identify crashed task, restarting all supervised tasks"
                            );
                            join_set.abort_all();
                            while join_set.join_next().await.is_some() {}
                            task_id_map.clear();
                            for kind in &all_kinds {
                                Self::spawn_tracked_task(
                                    &mut join_set,
                                    &mut task_id_map,
                                    *kind,
                                    self.clone(),
                                );
                            }
                        }
                    } else {
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
                        tracing::error!("Kernel exceeded restart budget, shutting down");
                        break;
                    }
                }
                None => {
                    // All tasks exited — should not happen
                    tracing::error!("All kernel tasks exited, shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Check if a task is within its restart budget. Returns true if restart is allowed.
    fn check_restart_budget(
        &self,
        counts: &mut std::collections::HashMap<String, (u32, std::time::Instant)>,
        task_name: &str,
    ) -> bool {
        let now = std::time::Instant::now();
        let entry = counts.entry(task_name.to_string()).or_insert((0, now));

        // Reset counter if outside the window
        if now.duration_since(entry.1) > Duration::from_secs(RESTART_WINDOW_SECS) {
            *entry = (1, now);
            return true;
        }

        entry.0 += 1;
        if entry.0 > MAX_RESTARTS {
            return false;
        }

        true
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
                                message: format!(
                                    "Per-agent rate limit exceeded. Retry after {} ms",
                                    wait.as_millis()
                                ),
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
            } => {
                self.cmd_connect_agent(name, provider, model, base_url, roles)
                    .await
            }
            KernelCommand::ListAgents => self.cmd_list_agents().await,
            KernelCommand::DisconnectAgent { agent_id } => {
                self.cmd_disconnect_agent(agent_id).await
            }
            KernelCommand::RunTask {
                agent_name, prompt, ..
            } => self.cmd_run_task(agent_name, prompt).await,
            KernelCommand::ListTasks => self.cmd_list_tasks().await,
            KernelCommand::SetSecret {
                name,
                value,
                scope,
                scope_raw,
            } => self.cmd_set_secret(name, value, scope, scope_raw).await,
            KernelCommand::ListSecrets => self.cmd_list_secrets().await,
            KernelCommand::RotateSecret { name, new_value } => {
                self.cmd_rotate_secret(name, new_value).await
            }
            KernelCommand::RevokeSecret { name } => self.cmd_revoke_secret(name).await,
            KernelCommand::GetTaskLogs { task_id } => self.cmd_get_task_logs(task_id).await,
            KernelCommand::CancelTask { task_id } => self.cmd_cancel_task(task_id).await,
            KernelCommand::ListTools => self.cmd_list_tools().await,
            KernelCommand::InstallTool { manifest_path } => {
                self.cmd_install_tool(manifest_path).await
            }
            KernelCommand::RemoveTool { tool_name } => self.cmd_remove_tool(tool_name).await,
            KernelCommand::GrantPermission {
                agent_name,
                permission,
            } => self.cmd_grant_permission(agent_name, permission).await,
            KernelCommand::RevokePermission {
                agent_name,
                permission,
            } => self.cmd_revoke_permission(agent_name, permission).await,
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

            KernelCommand::Shutdown => {
                tracing::info!("Shutdown command received, initiating graceful shutdown");
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::KernelShutdown,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "reason": "shutdown_command" }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                self.cancellation_token.cancel();
                agentos_bus::KernelResponse::Success {
                    data: Some(serde_json::json!({ "status": "shutting_down" })),
                }
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
                tracing::info!(job_name = %job.name, "Firing scheduled job");

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ScheduledJobFired,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "job_name": job.name }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                match self
                    .create_background_task(
                        job.name.clone(),
                        job.agent_name.clone(),
                        job.task_prompt.clone(),
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
                    }
                }
            }
        }
    }
}
