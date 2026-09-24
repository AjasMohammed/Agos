use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;
use std::collections::BTreeSet;
use std::time::Duration;

/// Timeout budget for a background task. Mirrors the branch `cmd_run_task` and
/// `api_submit_task` already apply — an autonomous task is expected to run past
/// the 1h non-autonomous default, and `check_timeouts` has no autonomous
/// exemption, so it must carry the autonomous budget on the task itself.
pub(crate) fn background_task_timeout(
    kernel: &crate::config::KernelSettings,
    autonomous: bool,
) -> Duration {
    if autonomous {
        Duration::from_secs(kernel.autonomous_mode.task_timeout_secs)
    } else {
        Duration::from_secs(kernel.default_task_timeout_secs)
    }
}

impl Kernel {
    pub(crate) async fn create_background_task(
        &self,
        name: String,
        agent_name: String,
        prompt: String,
        detached: bool,
        bounded: bool,
        // The agent that scheduled this work, when it isn't the operator.
        schedule_creator: Option<AgentID>,
    ) -> Result<TaskID, AgentOSError> {
        // Reject a name only while a task holding it is still live. Finished
        // entries linger in the pool for an hour (and a cancelled one's pool
        // state never leaves Queued), so checking mere presence failed every
        // recurring schedule firing more often than hourly. The scheduler, not
        // the pool, knows whether a task is still live.
        for existing in self.background_pool.named(&name).await {
            let live = self
                .scheduler
                .get_task(&existing.id)
                .await
                .is_some_and(|t| {
                    !matches!(
                        t.state,
                        TaskState::Complete | TaskState::Failed | TaskState::Cancelled
                    )
                });
            if live {
                return Err(AgentOSError::KernelError {
                    reason: format!("Background task '{}' already exists", name),
                });
            }
        }

        let registry = self.agent_registry.read().await;
        let agent = registry
            .get_by_name(&agent_name)
            .ok_or_else(|| AgentOSError::AgentNotFound(agent_name.clone()))?
            .clone();

        let mut target_permissions = clamp_to_schedule_creator(
            &registry,
            agent.id,
            registry.compute_effective_permissions(&agent.id),
            schedule_creator,
        );
        drop(registry);

        // Bounded tasks (schedule-fired RunTask, etc.) cap iterations to prevent
        // small-model tool-call loops. Unbounded tasks (user-launched
        // `agentos run-bg`) keep autonomous semantics.
        let (autonomous, max_iterations) = if bounded {
            (false, Some(10u32))
        } else {
            (true, None)
        };

        // SECURITY: only a genuinely autonomous task (operator-launched
        // `run-bg`) gets the implicit shell grant, exactly as `cmd_run_task`
        // gates it. A bounded task is schedule-fired and its prompt comes from
        // whatever asked for the schedule, so granting here turned
        // `schedule.job:w` / `schedule.timer:w` into a one-call route to
        // `process.exec` that no permission check ever saw — defeating the
        // "process.exec is never granted by default" invariant in
        // `default_permissions_for_agent`. An agent that legitimately needs
        // shell from a schedule holds `process.exec:x` explicitly.
        if autonomous {
            target_permissions.grant_op("process.exec".to_string(), PermissionOp::Execute, None);
        }
        // Same branch `cmd_run_task` / `api_submit_task` apply: an autonomous
        // task gets the autonomous budget, or the TimeoutChecker kills
        // long-running background work at the 1h non-autonomous default. The
        // capability token must outlive the task, so both use it.
        let task_timeout = background_task_timeout(&self.config.kernel, autonomous);

        let task_id = TaskID::new();
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
                target_permissions,
                task_timeout,
            )
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

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
            reasoning_hints: None,
            max_iterations,
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
            // Operator/schedule-originated root task.
            chain_depth: 0,
        };

        self.background_pool
            .register(BackgroundTask {
                id: task_id,
                name,
                agent_name,
                task_prompt: prompt,
                state: TaskState::Queued,
                started_at: Some(chrono::Utc::now()),
                completed_at: None,
                result: None,
                detached,
                scheduled_job_id: None,
            })
            .await;

        let _ = self.scheduler.enqueue(task).await;

        Ok(task_id)
    }

    pub(crate) async fn cmd_run_background(
        &self,
        name: String,
        agent_name: String,
        task: String,
        detach: bool,
    ) -> KernelResponse {
        match self
            .create_background_task(name.clone(), agent_name, task, detach, false, None)
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::BackgroundTaskStarted,
                    agent_id: None,
                    task_id: Some(id),
                    tool_id: None,
                    details: serde_json::json!({ "bg_name": name }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success {
                    data: Some(serde_json::json!({ "task_id": id.to_string() })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_list_background(&self) -> KernelResponse {
        KernelResponse::BackgroundPoolList(self.background_pool.list_all().await)
    }

    /// Resolve a background task by name or UUID string.
    async fn resolve_background_task(&self, name: &str) -> Option<BackgroundTask> {
        if let Some(t) = self.background_pool.get_by_name(name).await {
            return Some(t);
        }
        if let Ok(id) = name.parse::<TaskID>() {
            return self.background_pool.get_task(&id).await;
        }
        None
    }

    pub(crate) async fn cmd_get_background_logs(
        &self,
        name: String,
        _follow: bool,
    ) -> KernelResponse {
        if let Some(task) = self.resolve_background_task(&name).await {
            self.cmd_get_task_logs(task.id).await
        } else {
            KernelResponse::Error {
                message: format!("Background task '{}' not found", name),
            }
        }
    }

    pub(crate) async fn cmd_kill_background(&self, name: String) -> KernelResponse {
        if let Some(task) = self.resolve_background_task(&name).await {
            // Delegate to the shared cancel path. Flipping the scheduler state
            // alone is not a kill: the executor's ONLY cancellation-detection
            // point is `context_manager.get_context` returning `TaskNotFound`,
            // so leaving the context in place lets the loop run to completion
            // and fire every remaining tool side effect. `cmd_cancel_task` also
            // finishes the trace, releases the checkout/work item, cleans up
            // subscriptions and cascades to children.
            match self.cmd_cancel_task(task.id).await {
                KernelResponse::Success { .. } => {
                    self.background_pool
                        .fail(&task.id, "Killed by user".to_string())
                        .await;
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::BackgroundTaskKilled,
                        agent_id: None,
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({ "bg_name": task.name }),
                        severity: agentos_audit::AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    });
                    KernelResponse::Success { data: None }
                }
                other => other,
            }
        } else {
            KernelResponse::Error {
                message: format!("Background task '{}' not found", name),
            }
        }
    }
}

/// Permissions for work that agent `creator` scheduled to run as agent `target`.
///
/// A schedule fires long after the creating task (and its token) are gone, so
/// the delegation clamp `scope_child_task` can't be used. Same rule, applied at
/// fire time: a cross-agent schedule runs with `target ∩ creator` — it can never
/// do more than the agent that asked for it could do itself *right now*.
/// Operator-created (`None`) and self-targeted schedules are unchanged. A
/// creator that no longer exists has no permissions, so the result is empty.
pub(crate) fn clamp_to_schedule_creator(
    registry: &crate::agent_registry::AgentRegistry,
    target: AgentID,
    target_permissions: PermissionSet,
    creator: Option<AgentID>,
) -> PermissionSet {
    match creator {
        Some(c) if c != target => {
            target_permissions.intersect_with(&registry.compute_effective_permissions(&c))
        }
        _ => target_permissions,
    }
}

#[cfg(test)]
mod tests {
    use super::background_task_timeout;
    use crate::config::{KernelConfig, KernelSettings};
    use std::time::Duration;

    /// Parse the shipped defaults rather than hand-building settings — the point
    /// of the check is that the real config's autonomous budget is applied.
    fn shipped_kernel_settings() -> KernelSettings {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/default.toml");
        let content = std::fs::read_to_string(&path).expect("config/default.toml must exist");
        toml::from_str::<KernelConfig>(&content)
            .expect("config/default.toml must parse")
            .kernel
    }

    #[test]
    fn autonomous_background_task_gets_the_autonomous_timeout() {
        let k = shipped_kernel_settings();
        assert_eq!(
            background_task_timeout(&k, true),
            Duration::from_secs(k.autonomous_mode.task_timeout_secs)
        );
        assert_eq!(
            background_task_timeout(&k, false),
            Duration::from_secs(k.default_task_timeout_secs)
        );
        assert!(
            background_task_timeout(&k, true) > background_task_timeout(&k, false),
            "an unbounded `run-bg` task is autonomous — it must not be killed at \
             the 1h non-autonomous default"
        );
    }
}
