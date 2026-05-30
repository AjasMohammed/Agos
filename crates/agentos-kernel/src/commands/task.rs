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
        let task_timeout = if autonomous {
            Duration::from_secs(self.config.kernel.autonomous_mode.task_timeout_secs)
        } else {
            Duration::from_secs(self.config.kernel.default_task_timeout_secs)
        };
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
        match result {
            Ok(task_result) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                self.scheduler
                    .update_state_if_not_terminal(&task.id, TaskState::Complete)
                    .await
                    .ok();
                self.cleanup_task_subscriptions(&task.id).await;
                self.trace_collector
                    .finish_task(&task.id, "Complete", chrono::Utc::now())
                    .await;
                task_span.set_string_attribute("task.status", "complete");
                task_span.set_i64_attribute("task.iterations", task_result.iterations as i64);
                self.otel
                    .record_task_metric(&task.agent_id.to_string(), "complete", duration_ms);
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
                let is_waiting = self
                    .scheduler
                    .get_task(&task.id)
                    .await
                    .map(|t| t.state == TaskState::Waiting)
                    .unwrap_or(false);
                let paused_by_message = msg.to_ascii_lowercase().starts_with("task paused:");
                if is_waiting || paused_by_message {
                    self.scheduler
                        .update_state_if_not_terminal(&task.id, TaskState::Waiting)
                        .await
                        .ok();
                    self.trace_collector
                        .finish_task(&task.id, "Waiting", chrono::Utc::now())
                        .await;
                    task_span.set_string_attribute("task.status", "waiting");
                    task_span.record_error(&msg);
                    self.otel.record_task_metric(
                        &task.agent_id.to_string(),
                        "waiting",
                        duration_ms,
                    );
                    self.otel.adjust_active_tasks(-1);
                    return KernelResponse::Success {
                        data: Some(serde_json::json!({
                            "task_id": task.id.to_string(),
                            "status": "paused",
                            "reason": msg,
                        })),
                    };
                }

                self.scheduler
                    .update_state_if_not_terminal(&task.id, TaskState::Failed)
                    .await
                    .ok();
                self.cleanup_task_subscriptions(&task.id).await;
                self.trace_collector
                    .finish_task(&task.id, "Failed", chrono::Utc::now())
                    .await;
                task_span.set_string_attribute("task.status", "failed");
                task_span.record_error(&msg);
                self.otel
                    .record_task_metric(&task.agent_id.to_string(), "failed", duration_ms);
                self.otel.adjust_active_tasks(-1);
                KernelResponse::Error { message: msg }
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

        let child_permissions = parent_task.capability_token.permissions.clone();
        let mut effective_permissions = child_permissions.intersect(&target_permissions);

        // Autonomous delegated tasks get shell execution — mirrors the grant
        // in cmd_run_task / create_background_task so child agents can use
        // shell-exec without requiring it in their base permission set.
        if parent_task.autonomous {
            effective_permissions.grant_op("process.exec".to_string(), PermissionOp::Execute, None);
        }

        let child_token = self.capability_engine.issue_token(
            TaskID::new(),
            target.id,
            parent_task.capability_token.allowed_tools.clone(),
            parent_task.capability_token.allowed_intents.clone(),
            effective_permissions,
            Duration::from_secs(timeout_secs),
        )?;

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
            // Child tasks inherit the parent's autonomous mode so long-running
            // orchestrators don't have their sub-agents capped arbitrarily.
            autonomous: parent_task.autonomous,
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            spawner_agent_id: None,
            tool_categories: None,
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

        let _ = self.scheduler.enqueue(child_task.clone()).await;

        // Register the dependency: parent waits on child
        self.scheduler
            .add_dependency(parent_task.id, child_task.id)
            .await;

        // Emit TaskDelegated from the parent's perspective
        self.emit_event(
            EventType::TaskDelegated,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "parent_task_id": parent_task.id.to_string(),
                "child_task_id": child_task.id.to_string(),
                "parent_agent_id": parent_task.agent_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent_name,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            0,
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
            0,
        )
        .await;

        Ok(serde_json::json!({
            "delegated_to": target_agent_name,
            "child_task_id": child_task.id.to_string(),
            "status": "queued",
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
        // Enforce spawn depth limit — same cap as cmd_spawn_sub_agent.
        const MAX_SPAWN_DEPTH: u8 = 5;
        if spawner_task.spawn_depth >= MAX_SPAWN_DEPTH {
            return Err(AgentOSError::PermissionDenied {
                resource: "agent.spawn".to_string(),
                operation: format!(
                    "spawn depth limit ({MAX_SPAWN_DEPTH}) exceeded (current: {})",
                    spawner_task.spawn_depth
                ),
            });
        }

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

        let child_permissions = spawner_task.capability_token.permissions.clone();
        let effective_permissions = child_permissions.intersect(&target_permissions);

        let child_token = self.capability_engine.issue_token(
            TaskID::new(),
            target.id,
            spawner_task.capability_token.allowed_tools.clone(),
            spawner_task.capability_token.allowed_intents.clone(),
            effective_permissions,
            Duration::from_secs(timeout_secs),
        )?;

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
            spawn_depth: spawner_task.spawn_depth.saturating_add(1),
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            // Stored for future cross-task ownership queries (not yet used).
            spawner_agent_id: Some(spawner_task.agent_id),
            // Sub-agents inherit parent's allowlist (no widening allowed in this path).
            tool_categories: spawner_task.tool_categories.clone(),
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
                "parent_agent_id": spawner_task.agent_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent_name,
                "async": true,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            0,
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
            0,
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
        let task_timeout = Duration::from_secs(self.config.kernel.default_task_timeout_secs);
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
            trigger_source: None,
            autonomous: payload.task.autonomous,
            parent_task_id: payload.task.parent_task_id,
            spawn_depth: payload.task.spawn_depth,
            is_team_coordinator: payload.task.is_team_coordinator,
            skip_checkpoint: payload.task.skip_checkpoint,
            thinking_level: payload.task.thinking_level,
            spawner_agent_id: payload.task.spawner_agent_id,
            tool_categories: payload.task.tool_categories,
        };

        // 6. Restore context window from checkpoint.
        self.context_manager
            .replace_context(&task_id, payload.context.window)
            .await
            .ok();

        // 7. Enqueue the task.
        self.scheduler.register_external(resumed_task.clone()).await;

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
