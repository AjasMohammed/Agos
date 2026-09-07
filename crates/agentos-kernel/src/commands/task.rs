use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;
use std::collections::BTreeSet;
use std::time::Duration;

impl Kernel {
    pub(crate) async fn cmd_run_task(
        &self,
        agent_name: Option<String>,
        prompt: String,
        autonomous: bool,
        no_checkpoint: bool,
        thinking_level: ThinkingLevel,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent_id = match agent_name {
            Some(name) => match registry.get_by_name(&name) {
                Some(a) if a.status != AgentStatus::Offline => a.id,
                Some(_) => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' is offline", name),
                    }
                }
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' not found", name),
                    }
                }
            },
            None => {
                let agents: Vec<AgentProfile> =
                    registry.list_online().into_iter().cloned().collect();
                match self.router.route(&prompt, &agents).await {
                    Ok(id) => id,
                    Err(e) => {
                        return KernelResponse::Error {
                            message: format!("Failed to route task: {}", e),
                        }
                    }
                }
            }
        };

        let agent = match registry.get_by_id(&agent_id) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found after routing", agent_id),
                }
            }
        };
        let mut effective_permissions = registry.compute_effective_permissions(&agent_id);
        // Agent-level default thinking level applies when the caller did not request a
        // non-default value (legacy callers pass Off).
        let effective_thinking_level = if matches!(thinking_level, ThinkingLevel::Off) {
            agent.default_thinking_level.clone()
        } else {
            thinking_level
        };
        drop(registry);

        // Autonomous tasks get shell execution permission — interactive tasks do not
        if autonomous {
            effective_permissions.grant_op("process.exec".to_string(), PermissionOp::Execute, None);
        }

        let task_id = TaskID::new();
        let task_timeout = effective_task_timeout(
            autonomous,
            self.config.kernel.default_task_timeout_secs,
            self.config.kernel.autonomous_mode.task_timeout_secs,
        );
        let capability_token = match self.capability_engine.issue_token(
            task_id,
            agent.id,
            BTreeSet::new(),
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
            task_timeout,
        ) {
            Ok(token) => token,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to issue capability token: {}", e),
                };
            }
        };

        let reasoning_hints = Some(infer_reasoning_hints(&prompt));
        let task = AgentTask {
            id: task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token,
            assigned_llm: Some(agent.id),
            priority: 5,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: task_timeout,
            original_prompt: prompt,
            history: Vec::new(),
            parent_task: None,
            reasoning_hints,
            max_iterations: None,
            trigger_source: None,
            autonomous,
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: no_checkpoint,
            thinking_level: effective_thinking_level,
            spawner_agent_id: None,
            tool_categories: None,
            disable_tool_scoping: false,
            // Operator/CLI-originated root task.
            chain_depth: 0,
        };

        self.scheduler.register_external(task.clone()).await;
        self.scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Running)
            .await
            .ok();
        self.scheduler.mark_started(&task.id).await.ok();

        // Start trace accumulation before execution.
        self.trace_collector
            .start_task(task.id, agent.id, &task.original_prompt)
            .await;

        // Atomic single-owner checkout: claim the task before dispatch so no peer
        // agent can double-work it (crash-safe across restarts). The lease covers
        // the task's effective timeout plus a margin so a normal run never loses
        // its claim mid-flight; autonomous / zero-timeout tasks get a long default.
        let lease = if task.autonomous || task.timeout.is_zero() {
            std::time::Duration::from_secs(24 * 3600)
        } else {
            task.timeout + std::time::Duration::from_secs(300)
        };
        match self
            .task_checkout_store
            .try_claim(&task.id, &task.agent_id, lease)
            .await
        {
            Ok(true) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TaskCheckedOut,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({ "lease_secs": lease.as_secs() }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
            }
            Ok(false) => {
                // Already owned — expected race outcome, not an error. Leave the
                // task for its owner and return without dispatching.
                let owner = self
                    .task_checkout_store
                    .owner_of(&task.id)
                    .await
                    .ok()
                    .flatten();
                tracing::info!(task_id = %task.id, ?owner, "Task already checked out; skipping dispatch");
                return KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "task_id": task.id.to_string(),
                        "status": "already_owned",
                        "owner_agent_id": owner.map(|o| o.to_string()),
                    })),
                };
            }
            Err(e) => {
                // A store hiccup must not deny all work: the claim is a safety net,
                // not a hard gate. Log and proceed — single-owner still holds in the
                // common path; a genuine double-claim only risks the rare DB-down case.
                tracing::warn!(task_id = %task.id, error = %e, "Task checkout claim failed; proceeding without claim");
            }
        }

        // Execute task synchronously so the CLI gets the result
        let trace_id = TraceID::new();
        let start = std::time::Instant::now();
        let task_span = self.otel.start_task_span(
            &task.id.to_string(),
            &task.agent_id.to_string(),
            &agent.model,
        );
        self.otel.adjust_active_tasks(1);
        let result = self.execute_task_sync(&task, &trace_id, &task_span).await;
        // Terminal handling mirrors the background `execute_task` path: the shared
        // `complete_task_*` helpers own the state transition, checkout release,
        // subscription cleanup, episodic write, stored result, audit rows, WS
        // events, notification, work-item close and failure-streak breaker. They
        // are idempotent w.r.t. state (`update_state_if_not_terminal`) and are the
        // ONLY release/cleanup site now — do not re-add local copies here.
        match result {
            Ok(task_result) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                self.trace_collector
                    .finish_task(&task.id, "Complete", chrono::Utc::now())
                    .await;
                task_span.set_string_attribute("task.status", "complete");
                task_span.set_i64_attribute("task.iterations", task_result.iterations as i64);
                self.otel
                    .record_task_metric(&task.agent_id.to_string(), "complete", duration_ms);
                self.apply_memory_outcome(&task, true, trace_id).await;
                self.complete_task_success(&task, &task_result, duration_ms, trace_id)
                    .await;
                self.otel.adjust_active_tasks(-1);
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "task_id": task.id.to_string(),
                        "result": task_result.answer,
                    })),
                }
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                let msg = e.to_string();
                let task_state = self.scheduler.get_task(&task.id).await.map(|t| t.state);
                let paused = is_pause_outcome(task_state, &msg);
                if paused {
                    // `complete_task_failure` parks a Waiting task and returns without
                    // transitioning it, so the Waiting state has to be set here for the
                    // "Task paused:"-by-message case the executor never transitioned.
                    self.scheduler
                        .update_state_if_not_terminal(&task.id, TaskState::Waiting)
                        .await
                        .ok();
                }
                // Pair the `TaskStart` hook the executor fired — without this a failed
                // CLI task leaves hooks observing an unterminated task.
                self.hook_registry
                    .fire(&agentos_types::HookEvent::TaskEnd {
                        task_id: task.id,
                        agent_id: task.agent_id,
                        success: false,
                    })
                    .await;
                let status = if paused { "Waiting" } else { "Failed" };
                let metric = if paused { "waiting" } else { "failed" };
                self.trace_collector
                    .finish_task(&task.id, status, chrono::Utc::now())
                    .await;
                task_span.set_string_attribute("task.status", metric);
                task_span.record_error(&msg);
                self.otel
                    .record_task_metric(&task.agent_id.to_string(), metric, duration_ms);
                if !skips_memory_reinforcement(task_state, &msg) {
                    // Only a genuine failure reinforces: an unfinished task has not
                    // finished, so scoring its procedures would punish work still in
                    // flight.
                    self.apply_memory_outcome(&task, false, trace_id).await;
                }
                // Parks on Waiting/Suspended, otherwise records the failure streak and
                // runs the full terminal-failure path.
                self.complete_task_failure(&task, e, duration_ms, trace_id)
                    .await;
                self.otel.adjust_active_tasks(-1);
                if paused {
                    KernelResponse::Success {
                        data: Some(serde_json::json!({
                            "task_id": task.id.to_string(),
                            "status": "paused",
                            "reason": msg,
                        })),
                    }
                } else {
                    KernelResponse::Error { message: msg }
                }
            }
        }
    }

    /// Release a task's atomic checkout. Best-effort: a store error is logged,
    /// not propagated (the lease sweep is the backstop), and releasing a
    /// never-claimed task is a harmless no-op. Owner-scoped — a release only
    /// succeeds for the agent that holds the claim, so a stale caller cannot
    /// unlock a task another owner has since taken over.
    pub(crate) async fn release_task_checkout(&self, task_id: &TaskID, owner: &AgentID) {
        match self.task_checkout_store.release(task_id, owner).await {
            Ok(false) => {
                tracing::debug!(task_id = %task_id, owner = %owner, "Task checkout release: not owned by this agent");
            }
            Ok(true) => {}
            Err(e) => {
                tracing::warn!(task_id = %task_id, error = %e, "Task checkout release failed");
            }
        }
    }

    /// Close the autonomous work-loop for a terminal task: if this task was
    /// driving a claimed work item, mark it Done/Failed (success) and unblock its
    /// dependents. A no-op when the work queue is disabled or the task has no
    /// linked item, so it is safe to call on every task completion.
    pub(crate) async fn complete_work_item_for_task(&self, task_id: &TaskID, success: bool) {
        let Some(work_queue) = &self.work_queue else {
            return;
        };
        match work_queue
            .complete_by_task(&task_id.to_string(), success)
            .await
        {
            Ok(unblocked) if !unblocked.is_empty() => {
                tracing::info!(
                    task_id = %task_id,
                    unblocked = unblocked.len(),
                    "Work item completed; unblocked dependents"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(task_id = %task_id, error = %e, "Work item completion failed");
            }
        }
    }

    pub(crate) async fn cmd_list_tasks(&self) -> KernelResponse {
        let tasks = self.scheduler.list_tasks().await;
        KernelResponse::TaskList(tasks)
    }

    pub(crate) async fn cmd_get_task_logs(&self, task_id: TaskID) -> KernelResponse {
        // Verify task exists first
        if self.scheduler.get_task(&task_id).await.is_none() {
            return KernelResponse::Error {
                message: format!("Task '{}' not found", task_id),
            };
        }
        match self.audit.query_since_for_task(&task_id, 0, 500) {
            Ok(entries) => {
                let logs: Vec<String> = entries
                    .into_iter()
                    .map(|(_, entry)| {
                        format!(
                            "[{}] {:?} {}",
                            entry.timestamp.format("%H:%M:%S"),
                            entry.event_type,
                            entry.details,
                        )
                    })
                    .collect();
                KernelResponse::TaskLogs(logs)
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to query task logs: {}", e),
            },
        }
    }

    pub(crate) async fn cmd_cancel_task(&self, task_id: TaskID) -> KernelResponse {
        // Fetch the task before transitioning state so we have prompt + parent info.
        let task_snapshot = self.scheduler.get_task(&task_id).await;
        // Kept separately: the snapshot is consumed by the notification arm below,
        // and the owner-scoped checkout release still needs the owning agent.
        let owner_agent_id = task_snapshot.as_ref().map(|t| t.agent_id);
        match self
            .scheduler
            .update_state(&task_id, TaskState::Cancelled)
            .await
        {
            Ok(_) => {
                // Send cancel notification to user inbox (root tasks only).
                if let Some(task) = task_snapshot {
                    if Kernel::is_root_task(&task)
                        && self.config.notifications.notify_on_task_failed
                    {
                        let (last_tool, last_iter, obs_iter, obs_tools) =
                            self.gather_task_progress(&task.id).await;
                        let failure = crate::task_completion::FailureDetails {
                            reason: "cancelled".to_string(),
                            error_chain: vec!["Task was cancelled by user".to_string()],
                            last_tool,
                            last_iteration: last_iter,
                        };
                        self.send_completion_notification(
                            &task,
                            TaskOutcome::Cancelled,
                            "Task was cancelled by user",
                            obs_tools,
                            obs_iter,
                            0,
                            TraceID::new(),
                            Some(failure),
                        )
                        .await;
                    }
                }
                self.cleanup_task_subscriptions(&task_id).await;
                // Release in-memory context so the ContextManager map doesn't leak.
                self.context_manager.remove_context(&task_id).await;
                // Finalise trace so the active-trace map doesn't leak the entry.
                self.trace_collector
                    .finish_task(&task_id, "Cancelled", chrono::Utc::now())
                    .await;
                // Close any work item this task was driving (Failed) and release its
                // dispatch claim immediately rather than waiting for lock-TTL expiry.
                // Idempotent no-ops when there is no linked item / claim.
                self.complete_work_item_for_task(&task_id, false).await;
                if let Some(owner) = owner_agent_id {
                    self.release_task_checkout(&task_id, &owner).await;
                }
                // Cascade cancel to all registered sub-agent children.
                let children = self.scheduler.get_children(&task_id).await;
                for child_id in children {
                    // Use Box::pin to handle the recursive async call.
                    Box::pin(self.cmd_cancel_task(child_id)).await;
                }
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    /// Bulk-drop an agent's queued/waiting tasks.
    ///
    /// The 2026-07-26 trigger-loop incident left 190,426 queued tasks that were
    /// replayed on every boot; the only recovery was stopping the kernel and
    /// running `DELETE FROM scheduler_tasks` by hand. This does it live.
    pub(crate) async fn cmd_purge_tasks(
        &self,
        agent_id: AgentID,
        states: Vec<String>,
    ) -> KernelResponse {
        let mut parsed = Vec::new();
        for raw in &states {
            match raw.trim().to_ascii_lowercase().as_str() {
                "queued" => parsed.push(TaskState::Queued),
                "waiting" => parsed.push(TaskState::Waiting),
                "failed" => parsed.push(TaskState::Failed),
                "cancelled" | "canceled" => parsed.push(TaskState::Cancelled),
                "complete" | "completed" => parsed.push(TaskState::Complete),
                other => {
                    return KernelResponse::Error {
                        message: format!(
                            "Unknown task state '{other}'. Valid: queued, waiting, failed, \
                             cancelled, complete (running is never purged — use `task cancel`)"
                        ),
                    };
                }
            }
        }

        let purged = self.scheduler.purge_agent_tasks(&agent_id, &parsed).await;

        tracing::warn!(
            agent_id = %agent_id,
            purged,
            states = ?states,
            "Bulk-purged agent tasks"
        );
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::TaskStateChanged,
            agent_id: Some(agent_id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "action": "purge_tasks",
                "purged": purged,
                "states": states,
            }),
            severity: agentos_audit::AuditSeverity::Warn,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::Success {
            data: Some(serde_json::json!({ "purged": purged })),
        }
    }

    pub(crate) async fn cmd_get_task_trace(&self, task_id: TaskID) -> KernelResponse {
        match self.trace_collector.get_trace(&task_id).await {
            Ok(Some(trace)) => KernelResponse::TaskTrace(Box::new(trace)),
            Ok(None) => KernelResponse::Error {
                message: format!("No trace found for task '{}'", task_id),
            },
            Err(e) => KernelResponse::Error {
                message: format!("Failed to retrieve trace: {}", e),
            },
        }
    }

    pub(crate) async fn cmd_list_task_traces(
        &self,
        agent_id: Option<AgentID>,
        limit: u32,
    ) -> KernelResponse {
        match self.trace_collector.list_traces(agent_id, limit).await {
            Ok(summaries) => KernelResponse::TaskTraces(summaries),
            Err(e) => KernelResponse::Error {
                message: format!("Failed to list traces: {}", e),
            },
        }
    }

    pub(crate) async fn handle_task_delegation(
        &self,
        parent_task: &AgentTask,
        target_agent_name: &str,
        prompt: &str,
        priority: u8,
        timeout_secs: u64,
    ) -> Result<serde_json::Value, AgentOSError> {
        let registry = self.agent_registry.read().await;
        let target = registry
            .get_by_name(target_agent_name)
            .ok_or_else(|| AgentOSError::AgentNotFound(target_agent_name.to_string()))?
            .clone();

        if target.status == AgentStatus::Offline {
            return Err(AgentOSError::AgentNotFound(format!(
                "Agent '{}' is offline",
                target_agent_name
            )));
        }

        let target_permissions = registry.compute_effective_permissions(&target.id);
        drop(registry);

        // Scope the child through the shared, hardened path: depth cap, pure
        // parent∩target intersection (no process.exec re-grant), parent-token
        // signature/expiry verification, fresh child IDs.
        let (child_token, child_depth) = self
            .scope_child_task(
                parent_task,
                target.id,
                &target_permissions,
                Duration::from_secs(timeout_secs),
            )
            .await?;

        let child_task = AgentTask {
            id: child_token.task_id,
            state: TaskState::Queued,
            agent_id: target.id,
            capability_token: child_token,
            assigned_llm: None,
            priority,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(timeout_secs),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(parent_task.id),
            reasoning_hints: Some(infer_reasoning_hints(prompt)),
            max_iterations: None,
            trigger_source: None,
            // Children are always bounded — never inherit parent autonomy (a
            // 10k-iteration child is a fork-bomb amplifier). Matches every other
            // spawn path.
            autonomous: false,
            // Both parent fields set so cascade-cancel and is_root_task() agree.
            parent_task_id: Some(parent_task.id),
            spawn_depth: child_depth,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            spawner_agent_id: Some(parent_task.agent_id),
            tool_categories: parent_task.tool_categories.clone(),
            disable_tool_scoping: false,
            // Inherit the parent's causal depth so a delegated child cannot
            // restart the event-trigger chain counter at 0.
            chain_depth: parent_task.event_chain_depth(),
        };

        // Check for circular dependencies before enqueuing
        if let Err(reason) = self
            .scheduler
            .check_delegation_safe(parent_task.id, child_task.id)
            .await
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "task_delegation".to_string(),
                operation: reason,
            });
        }

        // Register the dependency: parent waits on child.
        self.scheduler
            .add_dependency(parent_task.id, child_task.id)
            .await;

        // Register for cascade-cancel: cancelling the parent must cancel this
        // delegated child too. add_dependency only tracks the wait edge; without
        // this the child is orphaned when the parent is cancelled.
        self.scheduler
            .register_child(parent_task.id, child_task.id)
            .await;

        // Park the parent BEFORE the child becomes runnable. `add_dependency`
        // alone never blocked anyone: the child's `complete_dependency` →
        // `requeue(parent)` wake is a deliberate no-op unless the parent is
        // already `Waiting` (see `TaskScheduler::requeue`), so a `Running`
        // parent consumed the edge and never observed the result. Parking
        // before `enqueue` closes the window in which a fast child could
        // finish while the parent is still `Running`.
        //
        // A caller with no scheduler-registered task (the chat path's synthetic
        // task, the MCP gateway) cannot be parked — it stays fire-and-forget and
        // reports `queued` so the returned status is never a lie.
        let parked = matches!(
            self.scheduler
                .update_state_if_not_terminal(&parent_task.id, TaskState::Waiting)
                .await,
            Ok(true)
        );

        let _ = self.scheduler.enqueue(child_task.clone()).await;

        // Emit TaskDelegated from the parent's perspective
        self.emit_event(
            EventType::TaskDelegated,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "parent_task_id": parent_task.id.to_string(),
                "child_task_id": child_task.id.to_string(),
                "agent_id": parent_task.agent_id.to_string(),
                "parent_agent_id": parent_task.agent_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent_name,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            parent_task.event_chain_depth(),
        )
        .await;

        // Emit DelegationReceived from the target agent's perspective
        self.emit_event(
            EventType::DelegationReceived,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "child_task_id": child_task.id.to_string(),
                "parent_task_id": parent_task.id.to_string(),
                "delegating_agent_id": parent_task.agent_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent_name,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            parent_task.event_chain_depth(),
        )
        .await;

        // `waiting_for_child` is the executor's signal to stop iterating (see
        // `Kernel::parked_on_delegation`); the child's completion requeues us.
        let (status, note) = if parked {
            (
                "waiting_for_child",
                "You are paused until this child finishes; its output is delivered into your context and you resume automatically.",
            )
        } else {
            (
                "queued",
                "Delegation is not blocking for this caller; poll with task-status.",
            )
        };
        Ok(serde_json::json!({
            "delegated_to": target_agent_name,
            "child_task_id": child_task.id.to_string(),
            "status": status,
            "note": note,
        }))
    }

    /// Fire-and-forget async spawn. No scheduler dependency is added so the spawning task
    /// continues without waiting. When the child completes, `inject_sub_agent_result` in
    /// `task_completion.rs` fires (triggered by `parent_task_id`) — but only if the spawner's
    /// context window is still active. Use `poll-agent` with the returned task_id for reliable
    /// status checks across task boundaries.
    pub(crate) async fn handle_spawn_async(
        &self,
        spawner_task: &AgentTask,
        target_agent_name: &str,
        prompt: &str,
        priority: u8,
        timeout_secs: u64,
    ) -> Result<serde_json::Value, AgentOSError> {
        let registry = self.agent_registry.read().await;
        let target = registry
            .get_by_name(target_agent_name)
            .ok_or_else(|| AgentOSError::AgentNotFound(target_agent_name.to_string()))?
            .clone();

        if target.status == AgentStatus::Offline {
            return Err(AgentOSError::AgentNotFound(format!(
                "Agent '{}' is offline",
                target_agent_name
            )));
        }

        let target_permissions = registry.compute_effective_permissions(&target.id);
        drop(registry);

        // Shared hardened scoping: depth cap, parent∩target, no exec re-grant,
        // parent-token verification.
        let (child_token, child_depth) = self
            .scope_child_task(
                spawner_task,
                target.id,
                &target_permissions,
                Duration::from_secs(timeout_secs),
            )
            .await?;

        let child_task = AgentTask {
            id: child_token.task_id,
            state: TaskState::Queued,
            agent_id: target.id,
            capability_token: child_token,
            // Must mirror cmd_spawn_sub_agent: set both parent_task AND parent_task_id so
            // cmd_cancel_task's root-task check and is_root_task() stay consistent.
            assigned_llm: Some(target.id),
            priority,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(timeout_secs),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(spawner_task.id),
            reasoning_hints: Some(infer_reasoning_hints(prompt)),
            max_iterations: None,
            trigger_source: None,
            // Sub-agents are always bounded — never inherit parent autonomy.
            autonomous: false,
            parent_task_id: Some(spawner_task.id),
            spawn_depth: child_depth,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            // Stored for future cross-task ownership queries (not yet used).
            spawner_agent_id: Some(spawner_task.agent_id),
            // Sub-agents inherit parent's allowlist (no widening allowed in this path).
            tool_categories: spawner_task.tool_categories.clone(),
            disable_tool_scoping: false,
            // Inherit the spawner's causal depth (see handle_task_delegation).
            chain_depth: spawner_task.event_chain_depth(),
        };

        let _ = self.scheduler.enqueue(child_task.clone()).await;
        // Register child for cascade-cancel: cancelling the spawner cancels this child too.
        self.scheduler
            .register_child(spawner_task.id, child_task.id)
            .await;
        // Intentionally no add_dependency — parent is NOT blocked.

        self.emit_event(
            EventType::TaskDelegated,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "parent_task_id": spawner_task.id.to_string(),
                "child_task_id": child_task.id.to_string(),
                "agent_id": spawner_task.agent_id.to_string(),
                "parent_agent_id": spawner_task.agent_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent_name,
                "async": true,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            spawner_task.event_chain_depth(),
        )
        .await;

        self.emit_event(
            EventType::DelegationReceived,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "child_task_id": child_task.id.to_string(),
                "parent_task_id": spawner_task.id.to_string(),
                "delegating_agent_id": spawner_task.agent_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent_name,
                "async": true,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            spawner_task.event_chain_depth(),
        )
        .await;

        Ok(serde_json::json!({
            "spawned_agent": target_agent_name,
            "task_id": child_task.id.to_string(),
            "status": "queued",
            "notification": "result injected into your context if still running; use poll-agent for reliable status",
        }))
    }

    /// Resume a task from its latest checkpoint.
    pub async fn cmd_resume_task(&self, task_id: TaskID) -> KernelResponse {
        // 0. A task the scheduler is already driving must not be resumed. The
        // checkout claim below is not sufficient on its own: a `Queued` task
        // has no claim row yet, and a long-running one may have had its lease
        // swept, so `try_claim` succeeds and the enqueue then runs a second
        // concurrent loop over the same task and context.
        if let Some(existing) = self.scheduler.get_task(&task_id).await {
            if matches!(existing.state, TaskState::Running | TaskState::Queued) {
                return KernelResponse::Error {
                    message: format!(
                        "Task '{task_id}' is already {:?}; resume refused",
                        existing.state
                    ),
                };
            }
        }

        // 1. Load the latest checkpoint.
        let record = match self.checkpoint_store.get_latest(&task_id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                return KernelResponse::Error {
                    message: format!("no checkpoint found for task '{}'", task_id),
                };
            }
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("failed to load checkpoint: {e}"),
                };
            }
        };

        // 2. Deserialize the checkpoint payload.
        let payload: crate::checkpoint_store::CheckpointPayload =
            match serde_json::from_slice(&record.state_blob) {
                Ok(p) => p,
                Err(e) => {
                    return KernelResponse::Error {
                        message: format!("failed to deserialize checkpoint payload: {e}"),
                    };
                }
            };

        // 3. Verify the agent still exists and is online.
        let agent = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_id(&payload.task.agent_id) {
                Some(a) if a.status != AgentStatus::Offline => a.clone(),
                Some(_) => {
                    return KernelResponse::Error {
                        message: format!(
                            "agent '{}' is offline — cannot resume task",
                            payload.task.agent_id
                        ),
                    };
                }
                None => {
                    return KernelResponse::Error {
                        message: format!(
                            "agent '{}' not found — cannot resume task",
                            payload.task.agent_id
                        ),
                    };
                }
            }
        };

        // 4. Issue a fresh capability token (old one may be expired).
        let effective_permissions = {
            let registry = self.agent_registry.read().await;
            registry.compute_effective_permissions(&agent.id)
        };
        // Resuming must not shrink the task's budget: an autonomous task keeps
        // the autonomous timeout it was created with (matches `cmd_run_task`),
        // otherwise a resumed 24h task died at the 1h interactive default.
        let task_timeout = effective_task_timeout(
            payload.task.autonomous,
            self.config.kernel.default_task_timeout_secs,
            self.config.kernel.autonomous_mode.task_timeout_secs,
        );
        // Preserve the checkpointed token's tool/intent scoping — resuming must
        // never BROADEN a task's privileges (e.g. a narrowed delegated child
        // must stay narrowed). Only the expiry is refreshed. The permission set
        // is re-derived from the live registry so revoked grants don't survive.
        let capability_token = match self.capability_engine.issue_token(
            task_id,
            agent.id,
            payload.task.capability_token.allowed_tools.clone(),
            payload.task.capability_token.allowed_intents.clone(),
            effective_permissions,
            task_timeout,
        ) {
            Ok(t) => t,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("failed to issue capability token for resumed task: {e}"),
                };
            }
        };

        // 5. Rebuild AgentTask with fresh token but preserved state.
        let resumed_task = AgentTask {
            id: task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token,
            assigned_llm: Some(agent.id),
            priority: payload.task.priority,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: task_timeout,
            original_prompt: payload.task.original_prompt,
            history: Vec::new(),
            parent_task: payload.task.parent_task,
            reasoning_hints: payload.task.reasoning_hints,
            max_iterations: payload.task.max_iterations,
            // Resuming restores the SAME task: dropping these silently widened
            // tool scoping and reset the event-trigger chain counter to 0.
            trigger_source: payload.task.trigger_source,
            autonomous: payload.task.autonomous,
            parent_task_id: payload.task.parent_task_id,
            spawn_depth: payload.task.spawn_depth,
            is_team_coordinator: payload.task.is_team_coordinator,
            skip_checkpoint: payload.task.skip_checkpoint,
            thinking_level: payload.task.thinking_level,
            spawner_agent_id: payload.task.spawner_agent_id,
            tool_categories: payload.task.tool_categories,
            disable_tool_scoping: payload.task.disable_tool_scoping,
            chain_depth: payload.task.chain_depth,
        };

        // 6. Restore context window from checkpoint. Upserts: the failure
        // teardown that made this task resumable also removed its context
        // entry, so a plain `replace_context` would silently restore nothing.
        self.context_manager
            .restore_context(task_id, agent.id, payload.context.window)
            .await;

        // 7. Re-claim the atomic checkout before re-dispatching. The original
        // claim may have been released (terminal) or swept (lease lapsed while
        // paused/crashed); re-claiming restores single-owner ownership. If another
        // owner already holds it, refuse the resume rather than double-running.
        let lease = if resumed_task.autonomous || resumed_task.timeout.is_zero() {
            std::time::Duration::from_secs(24 * 3600)
        } else {
            resumed_task.timeout + std::time::Duration::from_secs(300)
        };
        // A resume, unlike a first dispatch, has an existing owner by
        // definition: the run that checkpointed it. So `Err` (store busy /
        // locked) must REFUSE, not fall through — `cmd_run_task` proceeds
        // best-effort on `Err` because a fresh task has no owner to collide
        // with, and that reasoning does not carry over here.
        let claim = self
            .task_checkout_store
            .try_claim(&task_id, &resumed_task.agent_id, lease)
            .await;
        if resume_claim_refused(&claim) {
            let message = match claim {
                Err(e) => {
                    tracing::warn!(task_id = %task_id, error = %e, "Resume refused: checkout claim failed");
                    format!(
                        "Task '{task_id}' checkout claim failed ({e}); resume refused — retry once the store recovers"
                    )
                }
                _ => {
                    let owner = self
                        .task_checkout_store
                        .owner_of(&task_id)
                        .await
                        .ok()
                        .flatten();
                    tracing::info!(task_id = %task_id, ?owner, "Resume refused: task already checked out");
                    format!("Task '{task_id}' is already owned by another agent; resume refused")
                }
            };
            return KernelResponse::Error { message };
        }

        // 8. Enqueue the task onto the run queue so the background
        // task_executor_loop dequeues and executes it. `register_external`
        // only inserts into the task map without queueing, which would leave
        // the resumed task parked in Queued state forever (never executed).
        self.scheduler.enqueue(resumed_task.clone()).await;

        tracing::info!(
            task_id = %task_id,
            agent_name = %agent.name,
            step_restored = record.step_num,
            "Task resumed from checkpoint"
        );

        // 8. Audit entry.
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::CheckpointRestored,
            agent_id: Some(agent.id),
            task_id: Some(task_id),
            tool_id: None,
            details: serde_json::json!({
                "step_restored": record.step_num,
                "checkpoint_id": record.checkpoint_id,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "status": "resumed",
                "step_restored": record.step_num,
            })),
        }
    }

    /// List all tasks that have checkpoints available for resume.
    pub(crate) async fn cmd_list_checkpoints(&self) -> KernelResponse {
        match self.checkpoint_store.list_checkpoints().await {
            Ok(summaries) => {
                let entries: Vec<serde_json::Value> = summaries
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "task_id": s.task_id.to_string(),
                            "agent_id": s.agent_id.to_string(),
                            "step_num": s.step_num,
                            "checkpoint_id": s.checkpoint_id,
                            "updated_at": s.updated_at.to_rfc3339(),
                        })
                    })
                    .collect();
                KernelResponse::CheckpointList(entries)
            }
            Err(e) => KernelResponse::Error {
                message: format!("failed to list checkpoints: {e}"),
            },
        }
    }

    /// Submit a task asynchronously from the REST API — routes or resolves the
    /// agent, enqueues the task, and returns the task ID immediately without
    /// waiting for execution to complete.
    pub async fn api_submit_task(
        &self,
        agent_name: Option<String>,
        prompt: String,
        autonomous: bool,
    ) -> Result<TaskID, String> {
        let registry = self.agent_registry.read().await;
        let agent_id = match agent_name {
            Some(ref name) => match registry.get_by_name(name) {
                Some(a) if a.status != AgentStatus::Offline => a.id,
                Some(_) => return Err(format!("Agent '{}' is offline", name)),
                None => return Err(format!("Agent '{}' not found", name)),
            },
            None => {
                let agents: Vec<AgentProfile> =
                    registry.list_online().into_iter().cloned().collect();
                match self.router.route(&prompt, &agents).await {
                    Ok(id) => id,
                    Err(e) => return Err(format!("Failed to route task: {}", e)),
                }
            }
        };

        let agent: AgentProfile = match registry.get_by_id(&agent_id) {
            Some(a) => a.clone(),
            None => return Err(format!("Agent '{}' not found after routing", agent_id)),
        };
        let mut effective_permissions = registry.compute_effective_permissions(&agent_id);
        drop(registry);

        if autonomous {
            effective_permissions.grant_op("process.exec".to_string(), PermissionOp::Execute, None);
        }

        let task_id = TaskID::new();
        let task_timeout = if autonomous {
            Duration::from_secs(self.config.kernel.autonomous_mode.task_timeout_secs)
        } else {
            Duration::from_secs(self.config.kernel.default_task_timeout_secs)
        };
        let capability_token = self
            .capability_engine
            .issue_token(
                task_id,
                agent.id,
                BTreeSet::new(),
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
                task_timeout,
            )
            .map_err(|e| format!("Failed to issue capability token: {}", e))?;

        let task = AgentTask {
            id: task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token,
            assigned_llm: Some(agent.id),
            priority: 5,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: task_timeout,
            original_prompt: prompt.clone(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: Some(infer_reasoning_hints(&prompt)),
            max_iterations: None,
            trigger_source: None,
            autonomous,
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            spawner_agent_id: None,
            tool_categories: None,
            disable_tool_scoping: false,
            // REST API-originated root task.
            chain_depth: 0,
        };

        self.trace_collector
            .start_task(task.id, agent.id, &task.original_prompt)
            .await;

        let _ = self.scheduler.enqueue(task).await;

        Ok(task_id)
    }
}

/// True when a sync run's error means the task *parked* (awaiting an external
/// decision) rather than failed terminally.
///
/// Mirrors the early-return guard in `complete_task_failure`: a parked task keeps
/// its `Waiting` state and its checkout claim and must never be force-transitioned
/// to `Failed`. `Suspended` (budget enforcement) is deliberately NOT a park here —
/// `complete_task_failure` already returns early for it, and forcing `Waiting`
/// would clobber the suspended state. The "task paused:" prefix is not re-spelled;
/// `classify_task_failure` owns it.
fn is_pause_outcome(state: Option<TaskState>, error_message: &str) -> bool {
    matches!(state, Some(TaskState::Waiting))
        || Kernel::classify_task_failure(error_message).0 == "task_paused"
}

/// True when a failed sync run must NOT feed negative memory reinforcement.
///
/// Wider than [`is_pause_outcome`]: a budget-`Suspended` task is resumable and
/// unfinished for exactly the same reason a parked one is, so scoring its
/// procedures punishes work still in flight. It is excluded from
/// `is_pause_outcome` only because that predicate also drives the forced
/// `Waiting` transition, which must never clobber `Suspended`.
fn skips_memory_reinforcement(state: Option<TaskState>, error_message: &str) -> bool {
    is_pause_outcome(state, error_message) || matches!(state, Some(TaskState::Suspended))
}

/// The wall-clock budget a task runs under. Shared by first dispatch and
/// resume: a resumed autonomous task previously fell back to the interactive
/// default, so a 24h task started dying after an hour.
pub(crate) fn effective_task_timeout(
    autonomous: bool,
    default_secs: u64,
    autonomous_secs: u64,
) -> Duration {
    Duration::from_secs(if autonomous {
        autonomous_secs
    } else {
        default_secs
    })
}

/// Resume refuses on anything but a clean claim.
///
/// A first dispatch (`cmd_run_task`) deliberately proceeds on `Err`: a brand-new
/// task has no other owner to collide with, so a store hiccup must not deny all
/// work. A resume is the opposite case — the task has an owner by definition
/// (the run that checkpointed it), so an unreadable store means "unknown owner",
/// which must not be treated as "no owner".
pub(crate) fn resume_claim_refused(claim: &Result<bool, AgentOSError>) -> bool {
    !matches!(claim, Ok(true))
}

/// Infer reasoning hints from a prompt's characteristics.
pub(crate) fn infer_reasoning_hints(prompt: &str) -> TaskReasoningHints {
    let word_count = prompt.split_whitespace().count();

    let complexity = if word_count > 200 {
        ComplexityLevel::High
    } else if word_count > 50 {
        ComplexityLevel::Medium
    } else {
        ComplexityLevel::Low
    };

    let preemption = match complexity {
        ComplexityLevel::High => PreemptionLevel::High,
        ComplexityLevel::Medium => PreemptionLevel::Normal,
        ComplexityLevel::Low => PreemptionLevel::Low,
    };

    let preferred_turns = match complexity {
        ComplexityLevel::High => Some(10),
        ComplexityLevel::Medium => Some(5),
        ComplexityLevel::Low => Some(3),
    };

    TaskReasoningHints {
        estimated_complexity: complexity,
        preferred_turns,
        preemption_sensitivity: preemption,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        effective_task_timeout, is_pause_outcome, resume_claim_refused, skips_memory_reinforcement,
    };
    use agentos_types::{AgentOSError, TaskState};

    // `cmd_run_task` itself cannot be unit-tested in this crate — there is no
    // in-process Kernel constructor (kernel boot is covered only by tests/e2e),
    // so these cover the seam the sync path now branches on before handing off
    // to the shared `complete_task_*` helpers.

    #[test]
    fn waiting_state_or_paused_error_parks() {
        // Executor already transitioned to Waiting (escalation raised).
        assert!(is_pause_outcome(Some(TaskState::Waiting), "boom"));
        // Only the error message says so — the state must still be set to Waiting.
        assert!(is_pause_outcome(
            Some(TaskState::Running),
            "Task paused: awaiting approval"
        ));
        assert!(is_pause_outcome(None, "task paused: awaiting approval"));
    }

    #[test]
    fn suspended_and_ordinary_errors_are_not_parks() {
        // Suspended must not be forced to Waiting — complete_task_failure owns it.
        assert!(!is_pause_outcome(
            Some(TaskState::Suspended),
            "Task suspended: budget exceeded"
        ));
        assert!(!is_pause_outcome(
            Some(TaskState::Running),
            "LLM error: 500"
        ));
        assert!(!is_pause_outcome(
            Some(TaskState::Running),
            "tool call rejected: paused"
        ));
        assert!(!is_pause_outcome(None, ""));
    }

    #[test]
    fn suspended_tasks_skip_negative_reinforcement() {
        // Suspended is resumable and unfinished — punishing its procedures
        // scores work still in flight.
        assert!(skips_memory_reinforcement(
            Some(TaskState::Suspended),
            "Task suspended: budget exceeded"
        ));
        assert!(skips_memory_reinforcement(Some(TaskState::Waiting), "boom"));
        // A genuine failure still reinforces.
        assert!(!skips_memory_reinforcement(
            Some(TaskState::Running),
            "LLM error: 500"
        ));
    }

    #[test]
    fn resumed_autonomous_task_keeps_the_autonomous_timeout() {
        // K-11: resume used the interactive default unconditionally, so a
        // resumed 24h autonomous task timed out at 1h.
        assert_eq!(
            effective_task_timeout(true, 3600, 86_400),
            std::time::Duration::from_secs(86_400)
        );
        assert_eq!(
            effective_task_timeout(false, 3600, 86_400),
            std::time::Duration::from_secs(3600)
        );
    }

    #[test]
    fn resume_refuses_on_claim_error_and_on_existing_owner() {
        // K-09: only `Ok(true)` may proceed. `Err` used to fall through to
        // enqueue, double-running a task that already had an owner.
        assert!(!resume_claim_refused(&Ok(true)));
        assert!(resume_claim_refused(&Ok(false)));
        assert!(resume_claim_refused(&Err(AgentOSError::StorageError(
            "database is locked".to_string()
        ))));
    }

    #[test]
    fn delegation_park_reason_is_a_pause_not_a_failure() {
        // MA-04: the executor's bail after parking on a delegated child must be
        // classified as a pause, or `complete_task_failure` would terminally
        // fail the parent instead of leaving it Waiting for the child's wake.
        let reason = crate::kernel::Kernel::DELEGATION_PARK_REASON;
        assert!(is_pause_outcome(Some(TaskState::Waiting), reason));
        assert!(is_pause_outcome(None, reason));
        assert!(skips_memory_reinforcement(Some(TaskState::Waiting), reason));
    }
}
