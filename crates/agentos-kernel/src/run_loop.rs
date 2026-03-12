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
}

impl std::fmt::Display for TaskKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskKind::Acceptor => write!(f, "Acceptor"),
            TaskKind::Executor => write!(f, "TaskExecutor"),
            TaskKind::TimeoutChecker => write!(f, "TimeoutChecker"),
            TaskKind::Scheduler => write!(f, "AgentdScheduler"),
            TaskKind::EventDispatcher => write!(f, "EventDispatcher"),
        }
    }
}

/// Maximum restarts per task within the restart window before declaring degraded.
const MAX_RESTARTS: u32 = 5;
/// Window in which MAX_RESTARTS is counted (seconds).
const RESTART_WINDOW_SECS: u64 = 60;

impl Kernel {
    /// Spawn a kernel subsystem task into the JoinSet, returning its TaskKind tag.
    fn spawn_task(join_set: &mut JoinSet<TaskKind>, kind: TaskKind, kernel: Arc<Kernel>) {
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
                });
            }
            TaskKind::Executor => {
                join_set.spawn(async move {
                    kernel.task_executor_loop().await;
                    TaskKind::Executor
                });
            }
            TaskKind::TimeoutChecker => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    let mut tick: u64 = 0;
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                                kernel.scheduler.check_timeouts().await;

                                // Sweep expired resource locks (Spec §8)
                                kernel.resource_arbiter.sweep_expired().await;

                                // Sweep expired vault proxy tokens (Spec §3)
                                kernel.vault.sweep_expired_proxy_tokens();

                                // Sweep expired escalations — auto-deny (Spec §12)
                                let expired_escalations = kernel.escalation_manager.sweep_expired().await;
                                for (esc_id, task_id, blocking) in &expired_escalations {
                                    kernel.audit_log(agentos_audit::AuditEntry {
                                        timestamp: chrono::Utc::now(),
                                        trace_id: agentos_types::TraceID::new(),
                                        event_type: agentos_audit::AuditEventType::ActionForbidden,
                                        agent_id: None,
                                        task_id: Some(*task_id),
                                        tool_id: None,
                                        details: serde_json::json!({
                                            "escalation_id": esc_id,
                                            "auto_action": "deny",
                                            "reason": "escalation_expired",
                                            "blocking": blocking,
                                        }),
                                        severity: agentos_audit::AuditSeverity::Warn,
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
                                }
                            }
                        }
                    }
                    TaskKind::TimeoutChecker
                });
            }
            TaskKind::Scheduler => {
                join_set.spawn(async move {
                    kernel.agentd_loop().await;
                    TaskKind::Scheduler
                });
            }
            TaskKind::EventDispatcher => {
                let token = kernel.cancellation_token.clone();
                join_set.spawn(async move {
                    // Take ownership of the event receiver (only the first spawn succeeds)
                    let receiver = {
                        let mut guard = kernel.event_receiver.lock().await;
                        guard.take()
                    };
                    if let Some(mut rx) = receiver {
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
                    } else {
                        tracing::warn!("EventDispatcher: receiver already taken, idling");
                        token.cancelled().await;
                    }
                    TaskKind::EventDispatcher
                });
            }
        }
    }

    /// The main supervised run loop.
    ///
    /// Spawns 5 core tasks (acceptor, executor, timeout checker, scheduler, event dispatcher) and
    /// monitors them via a JoinSet. If any task panics or exits unexpectedly, it is
    /// restarted automatically. If a task exceeds MAX_RESTARTS within
    /// RESTART_WINDOW_SECS, the kernel logs a degraded status and shuts down so the
    /// container orchestrator can restart the process cleanly.
    pub async fn run(self: Arc<Self>) -> Result<(), anyhow::Error> {
        let mut join_set = JoinSet::new();

        // Track restart counts per task kind
        let mut restart_counts: std::collections::HashMap<String, (u32, std::time::Instant)> =
            std::collections::HashMap::new();

        // Spawn all 5 core tasks
        let all_kinds = [
            TaskKind::Acceptor,
            TaskKind::Executor,
            TaskKind::TimeoutChecker,
            TaskKind::Scheduler,
            TaskKind::EventDispatcher,
        ];

        for kind in &all_kinds {
            Self::spawn_task(&mut join_set, *kind, self.clone());
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
                        event_type: agentos_audit::AuditEventType::KernelStarted, // reusing as restart signal
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
                        Self::spawn_task(&mut join_set, kind, self.clone());
                    } else {
                        tracing::error!(task = %kind, "Task exceeded restart budget, kernel degraded");
                        break;
                    }
                }
                Some(Err(join_error)) => {
                    let task_name = if join_error.is_panic() {
                        tracing::error!("Kernel task panicked: {:?}", join_error);
                        // We can't easily determine which task panicked from the JoinError alone,
                        // so we log the panic and restart all missing tasks
                        "unknown_panic".to_string()
                    } else {
                        tracing::error!("Kernel task cancelled: {:?}", join_error);
                        "unknown_cancelled".to_string()
                    };

                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: agentos_types::TraceID::new(),
                        event_type: agentos_audit::AuditEventType::KernelStarted,
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
                        // Re-spawn all task types (since we lost track of which one failed)
                        let current_count = join_set.len();
                        let expected = all_kinds.len();
                        if current_count < expected {
                            // Respawn all to ensure completeness
                            for kind in &all_kinds {
                                if join_set.len() < expected {
                                    Self::spawn_task(&mut join_set, *kind, self.clone());
                                }
                            }
                        }
                    } else {
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
                    // Check rate limit before processing
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
            } => {
                self.cmd_connect_agent(name, provider, model, base_url)
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
            } => self.cmd_run_pipeline(name, input, detach).await,
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
                throttle,
                priority,
            } => {
                self.cmd_event_subscribe(agent_name, event_filter, throttle, priority)
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

            KernelCommand::Shutdown => {
                std::process::exit(0);
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

                let _ = self
                    .create_background_task(
                        job.name.clone(),
                        job.agent_name.clone(),
                        job.task_prompt.clone(),
                        true,
                    )
                    .await;
            }
        }
    }
}
