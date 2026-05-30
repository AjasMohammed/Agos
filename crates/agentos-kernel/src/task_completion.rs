use crate::kernel::Kernel;
use crate::task_executor::TaskResult;
use crate::task_summary::{deduplicate_title, generate_task_summary};
use agentos_types::*;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Extra context attached to non-success notifications so the user gets a
/// "why / where / when" summary instead of a single-line error.
///
/// `last_tool` and `last_iteration` are best-effort: filled in from episodic
/// memory when the task ran at least one tool call, otherwise `None`.
#[derive(Debug, Clone, Default)]
pub(crate) struct FailureDetails {
    /// Classification produced by `Self::classify_task_failure` —
    /// `task_error`, `llm_error`, `max_iterations`, `budget_exceeded`, etc.
    pub reason: String,
    /// Anyhow error chain, root-cause-first (index 0 is the surfaced error).
    pub error_chain: Vec<String>,
    /// Most recent tool the agent invoked before failing, if any.
    pub last_tool: Option<String>,
    /// 1-based iteration index of `last_tool`, matching the `Iterations`
    /// metric reported on success (executor's `completed_iterations`).
    pub last_iteration: Option<u32>,
}

impl Kernel {
    /// Handle successful task completion: record to episodic memory, update scheduler state,
    /// emit events, notify background pool, wake dependency waiters, and trigger consolidation.
    #[tracing::instrument(skip_all, fields(task_id = %task.id, agent_id = %task.agent_id))]
    pub(crate) async fn complete_task_success(
        &self,
        task: &AgentTask,
        result: &TaskResult,
        duration_ms: u64,
        task_trace_id: TraceID,
    ) {
        tracing::info!("Task {} complete: {}", task.id, result.answer);
        crate::metrics::record_task_completed(duration_ms, true);

        // Record compact task success to episodic memory (token-efficient format)
        let summary_preview = format!(
            "task:{}\nresult:success|tools:{}|iters:{}|{}ms\nanswer:{}",
            Self::truncate_for_prompt_payload(&task.original_prompt, 200),
            result.tool_call_count,
            result.iterations,
            duration_ms,
            Self::truncate_for_prompt_payload(&result.answer, 500)
        );
        match self
            .episodic_memory
            .record(agentos_memory::EpisodeRecordInput {
                task_id: &task.id,
                agent_id: &task.agent_id,
                entry_type: agentos_memory::EpisodeType::SystemEvent,
                content: &summary_preview,
                summary: Some("Task completed successfully"),
                metadata: Some(serde_json::json!({
                    "outcome": "success",
                    "duration_ms": duration_ms,
                    "tool_calls": result.tool_call_count,
                    "iterations": result.iterations,
                })),
                trace_id: &task_trace_id,
            })
            .await
        {
            Ok(_) => {
                self.emit_event_with_trace(
                    EventType::EpisodicMemoryWritten,
                    EventSource::MemoryArbiter,
                    EventSeverity::Info,
                    serde_json::json!({
                        "task_id": task.id.to_string(),
                        "agent_id": task.agent_id.to_string(),
                        "entry_type": "task_completion",
                        "summary": summary_preview.chars().take(200).collect::<String>(),
                    }),
                    0,
                    Some(task_trace_id),
                    Some(task.agent_id),
                    Some(task.id),
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "Failed to record task completion");
            }
        }

        // Only transition to Complete and emit events if the task hasn't
        // been marked terminal by the timeout checker while we were running.
        let completed = self
            .scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Complete)
            .await
            .unwrap_or_else(|e| {
                tracing::error!(
                    task_id = %task.id,
                    error = %e,
                    "Scheduler error during task completion state transition — completion events skipped"
                );
                false
            });

        if completed {
            self.push_status_update(
                task.id,
                TaskState::Complete,
                "Task completed successfully".to_string(),
            );
            self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: task_trace_id,
                event_type: agentos_audit::AuditEventType::TaskCompleted,
                agent_id: Some(task.agent_id),
                task_id: Some(task.id),
                tool_id: None,
                details: serde_json::json!({
                    "iterations": result.iterations,
                    "tool_calls": result.tool_call_count,
                    "duration_ms": duration_ms,
                    "answer_preview": Self::truncate_for_prompt_payload(&result.answer, 200),
                }),
                severity: agentos_audit::AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            });
            self.emit_event_with_trace(
                EventType::TaskCompleted,
                EventSource::TaskScheduler,
                EventSeverity::Info,
                serde_json::json!({
                    "task_id": task.id.to_string(),
                    "agent_id": task.agent_id.to_string(),
                    "iterations": result.iterations,
                    "tool_calls": result.tool_call_count,
                }),
                0,
                Some(task_trace_id),
                Some(task.agent_id),
                Some(task.id),
            )
            .await;

            // Read scheduled_job_id before complete() to avoid a second lock acquisition
            let scheduled_job_id = self
                .background_pool
                .get_task(&task.id)
                .await
                .and_then(|bg| bg.scheduled_job_id);

            self.background_pool
                .complete(&task.id, serde_json::json!({ "result": result.answer }))
                .await;

            // If this was a scheduled task, emit ScheduledTaskCompleted +
            // transition the matching Running ScheduledRun to Complete + dispatch
            // the run's delivery (Direct / ViaAgent / Silent).
            if let Some(schedule_id) = scheduled_job_id {
                if let Some(job) = self.schedule_manager.get_job(&schedule_id).await {
                    self.schedule_manager.emit_task_completed(&job).await;
                    self.agent_inbox_writer
                        .write_scheduled(
                            task.agent_id,
                            task.id,
                            &job.name,
                            true,
                            serde_json::json!({ "result": result.answer }),
                        )
                        .await;
                }
                if let Some(store) = self.schedule_manager.store() {
                    // Race-free lookup: pending_runs map is populated by run_loop
                    // synchronously before the task starts. Falls back to an
                    // indexed SQLite query if the map miss occurs (e.g. kernel
                    // restart between task spawn and completion).
                    let mut run_opt = match self.schedule_manager.take_pending_run(&task.id).await {
                        Some(run_id) => match store.get_run(run_id).await {
                            Ok(Some(r)) => Some(r),
                            _ => None,
                        },
                        None => store
                            .find_running_run_for_task(task.id)
                            .await
                            .ok()
                            .flatten(),
                    };
                    if let Some(ref mut run) = run_opt {
                        run.state = agentos_types::schedule::RunState::Complete;
                        run.completed_at = Some(chrono::Utc::now());
                        run.result = Some(serde_json::json!({ "result": result.answer }));
                        run.tool_calls = result.tool_calls.clone();
                        let run_id = run.run_id;
                        if let Err(e) = store.upsert_run(run.clone()).await {
                            tracing::warn!(error = %e, "Failed to mark ScheduledRun as Complete");
                        } else {
                            self.dispatch_scheduled_delivery(run_id).await;
                        }
                    }
                }
            }

            // If this was an RPC child task, deliver the result to the blocked caller.
            // complete_call is a no-op if the task is not an RPC child.
            self.rpc_manager
                .complete_call(
                    &task.id,
                    crate::rpc_manager::RpcResult {
                        output: result.answer.clone(),
                        success: true,
                        error: None,
                    },
                )
                .await;

            // Wake any parent tasks that were waiting on this child
            let waiters = self.scheduler.complete_dependency(task.id).await;
            for waiter_id in &waiters {
                self.emit_event_with_trace(
                    EventType::DelegationResponseReceived,
                    EventSource::TaskScheduler,
                    EventSeverity::Info,
                    serde_json::json!({
                        "parent_task_id": waiter_id.to_string(),
                        "child_task_id": task.id.to_string(),
                        "child_agent_id": task.agent_id.to_string(),
                        "outcome": "success",
                    }),
                    0,
                    Some(task_trace_id),
                    Some(task.agent_id),
                    Some(task.id),
                )
                .await;
                if let Err(e) = self.scheduler.requeue(waiter_id).await {
                    tracing::warn!(error = %e, waiter_id = %waiter_id, "Requeue failed after task success — waiter will timeout naturally");
                }
            }

            // Trigger consolidation bookkeeping in the background.
            // Wire the kernel cancellation token so this task doesn't outlive
            // a graceful shutdown.
            let consolidation = self.consolidation_engine.clone();
            let token = self.cancellation_token.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = token.cancelled() => {}
                    _ = consolidation.on_task_completed() => {}
                }
            });
        } else {
            tracing::info!(
                task_id = %task.id,
                "Task finished but was already in terminal state (likely timed out), skipping completion"
            );
        }

        // Send task-completion notification to user inbox (root tasks only).
        if completed
            && Self::is_root_task(task)
            && self.config.notifications.notify_on_task_complete
        {
            self.send_completion_notification(
                task,
                TaskOutcome::Success,
                &result.answer,
                Some(result.tool_call_count),
                Some(result.iterations),
                duration_ms,
                task_trace_id,
                None,
            )
            .await;
        }

        // Auto-write scratchpad note for completed task
        if completed {
            self.auto_write_scratchpad_note(task, "Success").await;
        }

        // If this is a sub-agent task, inject its result into the parent context.
        if completed {
            if let Some(parent_task_id) = task.parent_task_id {
                let agent_name = {
                    let registry = self.agent_registry.read().await;
                    registry
                        .get_by_id(&task.agent_id)
                        .map(|a| a.name.clone())
                        .unwrap_or_else(|| task.agent_id.to_string())
                };
                let sub_result = agentos_types::SubAgentResult {
                    child_task_id: task.id,
                    agent_name: agent_name.clone(),
                    output: result.answer.chars().take(8192).collect(),
                    success: true,
                };

                if let Some(parent) = self.scheduler.get_task(&parent_task_id).await {
                    self.agent_inbox_writer
                        .write_async_done(
                            parent.agent_id,
                            task.id,
                            &agent_name,
                            true,
                            serde_json::json!({ "result": result.answer }),
                        )
                        .await;
                }
                if let Err(e) = self
                    .context_manager
                    .inject_sub_agent_result(parent_task_id, &sub_result)
                    .await
                {
                    tracing::warn!(
                        parent_task_id = %parent_task_id,
                        child_task_id = %task.id,
                        error = %e,
                        "Failed to inject sub-agent result into parent context"
                    );
                }
            }
        }

        // Resolve any escalations still pending for this task so the sweeper
        // doesn't auto-approve/deny them after the task has already finished.
        self.escalation_manager
            .resolve_for_task(&task.id, "Task completed successfully")
            .await;

        self.cleanup_task_subscriptions(&task.id).await;
    }

    /// Handle task failure: classify the error, record to episodic memory, update scheduler state,
    /// emit events, notify background pool, and clean up dependency edges.
    #[tracing::instrument(skip_all, fields(task_id = %task.id, agent_id = %task.agent_id))]
    pub(crate) async fn complete_task_failure(
        &self,
        task: &AgentTask,
        error: anyhow::Error,
        duration_ms: u64,
        task_trace_id: TraceID,
    ) {
        // Build the full anyhow error chain before consuming `error`.
        let error_chain: Vec<String> = error.chain().map(|e| e.to_string()).collect();
        let error_message = error_chain[0].clone();
        let (reason, severity, is_pause) = Self::classify_task_failure(&error_message);
        let task_state = self.scheduler.get_task(&task.id).await.map(|t| t.state);
        let task_is_waiting = matches!(task_state, Some(TaskState::Waiting));
        let task_is_suspended = matches!(task_state, Some(TaskState::Suspended));

        // Suspended tasks have already had their state set to Suspended by the executor.
        // Record episodic memory and return — do NOT transition to Failed.
        if task_is_suspended {
            tracing::info!(
                task_id = %task.id,
                "Task suspended due to budget enforcement: {}",
                error_message
            );
            if let Err(err) = self
                .episodic_memory
                .record(agentos_memory::EpisodeRecordInput {
                    task_id: &task.id,
                    agent_id: &task.agent_id,
                    entry_type: agentos_memory::EpisodeType::SystemEvent,
                    content: &format!(
                        "Task suspended: {}\nReason: {}",
                        task.original_prompt, error_message
                    ),
                    summary: Some("Task suspended due to budget enforcement"),
                    metadata: Some(serde_json::json!({
                        "outcome": "suspended",
                        "reason": error_message,
                    })),
                    trace_id: &task_trace_id,
                })
                .await
            {
                tracing::warn!(task_id = %task.id, error = %err, "Failed to record suspended task state");
            }
            // Notify RPC caller if this was an RPC child task (prevents parent hang)
            self.rpc_manager
                .complete_call(
                    &task.id,
                    crate::rpc_manager::RpcResult {
                        output: String::new(),
                        success: false,
                        error: Some(format!("RPC child task suspended: {}", error_message)),
                    },
                )
                .await;
            self.cleanup_task_subscriptions(&task.id).await;
            return;
        }

        if is_pause || task_is_waiting {
            tracing::info!(
                "Task {} paused and waiting for external decision: {}",
                task.id,
                error_message
            );
            if let Err(err) = self
                .episodic_memory
                .record(agentos_memory::EpisodeRecordInput {
                    task_id: &task.id,
                    agent_id: &task.agent_id,
                    entry_type: agentos_memory::EpisodeType::SystemEvent,
                    content: &format!(
                        "Task paused: {}\nReason: {}",
                        task.original_prompt, error_message
                    ),
                    summary: Some("Task paused awaiting external decision"),
                    metadata: Some(serde_json::json!({
                        "outcome": "paused",
                        "reason": error_message,
                    })),
                    trace_id: &task_trace_id,
                })
                .await
            {
                tracing::warn!(task_id = %task.id, error = %err, "Failed to record paused task state");
            }
            self.background_pool
                .set_waiting(&task.id, error_message.clone())
                .await;
            // Notify RPC caller if this was an RPC child task (prevents parent hang)
            self.rpc_manager
                .complete_call(
                    &task.id,
                    crate::rpc_manager::RpcResult {
                        output: String::new(),
                        success: false,
                        error: Some(format!("RPC child task paused: {}", error_message)),
                    },
                )
                .await;
            return;
        }

        if error_chain.len() > 1 {
            tracing::error!(
                task_id = %task.id,
                agent_id = %task.agent_id,
                duration_ms = duration_ms,
                prompt = %task.original_prompt.chars().take(120).collect::<String>(),
                error = %error_message,
                cause = %error_chain[1..].join(" → "),
                "Task failed"
            );
        } else {
            tracing::error!(
                task_id = %task.id,
                agent_id = %task.agent_id,
                duration_ms = duration_ms,
                prompt = %task.original_prompt.chars().take(120).collect::<String>(),
                error = %error_message,
                "Task failed"
            );
        }
        crate::metrics::record_task_completed(duration_ms, false);

        // Only transition to Failed and emit events if the task hasn't
        // been marked terminal by the timeout checker while we were running.
        let failed = self
            .scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Failed)
            .await
            .unwrap_or_else(|e| {
                tracing::error!(
                    task_id = %task.id,
                    error = %e,
                    "Scheduler error during task failure state transition — failure events skipped"
                );
                false
            });

        if !failed {
            tracing::info!(
                task_id = %task.id,
                "Task error but already in terminal state (likely timed out), skipping failure handling"
            );
            self.cleanup_task_subscriptions(&task.id).await;
            return;
        }

        self.push_status_update(task.id, TaskState::Failed, error_message.clone());

        // Inject the failure result into the parent context so the parent LLM
        // learns about the child failure through its context window, not just
        // via a subsequent await_agents poll.
        if let Some(parent_task_id) = task.parent_task_id {
            let agent_name = {
                self.agent_registry
                    .read()
                    .await
                    .get_by_id(&task.agent_id)
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| task.agent_id.to_string())
            };
            let failure_output = format!(
                "Sub-agent failed: {}\nError: {}",
                task.original_prompt.chars().take(200).collect::<String>(),
                error_message
            );
            let sub_result = agentos_types::SubAgentResult {
                child_task_id: task.id,
                agent_name: agent_name.clone(),
                output: failure_output.chars().take(8192).collect(),
                success: false,
            };

            if let Some(parent) = self.scheduler.get_task(&parent_task_id).await {
                self.agent_inbox_writer
                    .write_async_done(
                        parent.agent_id,
                        task.id,
                        &agent_name,
                        false,
                        serde_json::json!({ "error": error_message }),
                    )
                    .await;
            }
            match self
                .context_manager
                .inject_sub_agent_result(parent_task_id, &sub_result)
                .await
            {
                Ok(()) => {
                    tracing::debug!(
                        parent_task_id = %parent_task_id,
                        child_task_id = %task.id,
                        agent_name = %agent_name,
                        "Injected sub-agent failure into parent context"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        parent_task_id = %parent_task_id,
                        child_task_id = %task.id,
                        error = %e,
                        "Failed to inject sub-agent failure into parent context"
                    );
                }
            }
        }

        // Write a direct TaskFailed audit entry with task_id, agent_id, and full
        // error details. This is separate from the EventEmitted path below so the
        // failure is always queryable by task_id regardless of event-system state.
        // Cap each error-chain element so audit subscribers (webhooks, log
        // pipelines) don't receive unbounded strings — long anyhow chains can
        // easily embed full prompt text or large stack-like context.
        let error_chain_capped: Vec<String> = error_chain
            .iter()
            .map(|e| {
                let mut s: String = e.chars().take(300).collect();
                if e.chars().count() > 300 {
                    s.push('…');
                }
                s
            })
            .collect();
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: task_trace_id,
            event_type: agentos_audit::AuditEventType::TaskFailed,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "reason": reason,
                "error": error_message.chars().take(300).collect::<String>(),
                "error_chain": error_chain_capped,
                "duration_ms": duration_ms,
                "prompt_preview": task.original_prompt.chars().take(200).collect::<String>(),
            }),
            severity: agentos_audit::AuditSeverity::Error,
            reversible: false,
            rollback_ref: None,
        });

        // Emit TaskRetrying for retryable failure types (LLM transient errors).
        // Note: the task is not actually retried in this code path — this signal
        // indicates the failure *was* retryable, allowing subscribers to implement
        // their own retry logic or alerting.
        if reason == "llm_error" {
            self.emit_event_with_trace(
                EventType::TaskRetrying,
                EventSource::TaskScheduler,
                EventSeverity::Warning,
                serde_json::json!({
                    "task_id": task.id.to_string(),
                    "agent_id": task.agent_id.to_string(),
                    "reason": error_message,
                    "retry_eligible": true,
                    "action": "failed_without_retry",
                }),
                0,
                Some(task_trace_id),
                Some(task.agent_id),
                Some(task.id),
            )
            .await;
        }

        self.emit_event_with_trace(
            EventType::TaskFailed,
            EventSource::TaskScheduler,
            severity,
            serde_json::json!({
                "task_id": task.id.to_string(),
                "agent_id": task.agent_id.to_string(),
                "reason": reason,
                "error": error_message,
            }),
            0,
            Some(task_trace_id),
            Some(task.agent_id),
            Some(task.id),
        )
        .await;

        // Read scheduled_job_id before fail() to avoid a second lock acquisition
        let scheduled_job_id_on_failure = self
            .background_pool
            .get_task(&task.id)
            .await
            .and_then(|bg| bg.scheduled_job_id);

        self.background_pool
            .fail(&task.id, error_message.clone())
            .await;

        // If this was a scheduled task, emit ScheduledTaskFailed + transition
        // the matching Running ScheduledRun to Failed + dispatch its delivery
        // so the creator agent / target sees the failure too.
        if let Some(schedule_id) = scheduled_job_id_on_failure {
            if let Some(job) = self.schedule_manager.get_job(&schedule_id).await {
                self.schedule_manager
                    .emit_task_failed(&job, &error_message)
                    .await;
                self.agent_inbox_writer
                    .write_scheduled(
                        task.agent_id,
                        task.id,
                        &job.name,
                        false,
                        serde_json::json!({ "error": error_message }),
                    )
                    .await;
            }
            if let Some(store) = self.schedule_manager.store() {
                // Race-free lookup via pending_runs map (see complete_task_success).
                let mut run_opt = match self.schedule_manager.take_pending_run(&task.id).await {
                    Some(run_id) => match store.get_run(run_id).await {
                        Ok(Some(r)) => Some(r),
                        _ => None,
                    },
                    None => store
                        .find_running_run_for_task(task.id)
                        .await
                        .ok()
                        .flatten(),
                };
                let _ = schedule_id;
                if let Some(ref mut run) = run_opt {
                    run.state = agentos_types::schedule::RunState::Failed;
                    run.completed_at = Some(chrono::Utc::now());
                    run.error = Some(error_message.clone());
                    let run_id = run.run_id;
                    if let Err(e) = store.upsert_run(run.clone()).await {
                        tracing::warn!(error = %e, "Failed to mark ScheduledRun as Failed");
                    } else {
                        self.dispatch_scheduled_delivery(run_id).await;
                    }
                }
            }
        }

        let failure_summary = format!(
            "task:{}\nresult:failed|{}ms\nerror:{}",
            Self::truncate_for_prompt_payload(&task.original_prompt, 200),
            duration_ms,
            Self::truncate_for_prompt_payload(&error_message, 300)
        );
        match self
            .episodic_memory
            .record(agentos_memory::EpisodeRecordInput {
                task_id: &task.id,
                agent_id: &task.agent_id,
                entry_type: agentos_memory::EpisodeType::SystemEvent,
                content: &failure_summary,
                summary: Some("Task failed"),
                metadata: Some(serde_json::json!({ "outcome": "failure", "error": error_message })),
                trace_id: &task_trace_id,
            })
            .await
        {
            Ok(_) => {
                self.emit_event_with_trace(
                    EventType::EpisodicMemoryWritten,
                    EventSource::MemoryArbiter,
                    EventSeverity::Info,
                    serde_json::json!({
                        "task_id": task.id.to_string(),
                        "agent_id": task.agent_id.to_string(),
                        "entry_type": "task_failure",
                        "summary": failure_summary.chars().take(200).collect::<String>(),
                    }),
                    0,
                    Some(task_trace_id),
                    Some(task.agent_id),
                    Some(task.id),
                )
                .await;
            }
            Err(err) => {
                tracing::warn!(task_id = %task.id, error = %err, "Failed to record episodic memory");
            }
        }

        // Auto-write scratchpad note for failed task
        self.auto_write_scratchpad_note(task, "Failed").await;

        // If this was an RPC child task, deliver the failure to the blocked caller.
        // complete_call is a no-op if the task is not an RPC child.
        self.rpc_manager
            .complete_call(
                &task.id,
                crate::rpc_manager::RpcResult {
                    output: String::new(),
                    success: false,
                    error: Some(error_message.clone()),
                },
            )
            .await;

        // Clean up dependency edges even on failure
        let waiters = self.scheduler.complete_dependency(task.id).await;
        for waiter_id in &waiters {
            self.emit_event_with_trace(
                EventType::DelegationResponseReceived,
                EventSource::TaskScheduler,
                EventSeverity::Info,
                serde_json::json!({
                    "parent_task_id": waiter_id.to_string(),
                    "child_task_id": task.id.to_string(),
                    "child_agent_id": task.agent_id.to_string(),
                    "outcome": "failure",
                }),
                0,
                Some(task_trace_id),
                Some(task.agent_id),
                Some(task.id),
            )
            .await;
            if let Err(e) = self.scheduler.requeue(waiter_id).await {
                tracing::warn!(error = %e, waiter_id = %waiter_id, "Requeue failed after task failure — waiter will timeout naturally");
            }
        }

        // Send task-failure notification to user inbox (root tasks only).
        if Self::is_root_task(task) && self.config.notifications.notify_on_task_failed {
            let (last_tool, last_iteration, observed_iterations, observed_tool_calls) =
                self.gather_task_progress(&task.id).await;
            let failure = FailureDetails {
                reason: reason.to_string(),
                error_chain: error_chain.clone(),
                last_tool,
                last_iteration,
            };
            self.send_completion_notification(
                task,
                TaskOutcome::Failed,
                &error_message,
                observed_tool_calls,
                observed_iterations,
                duration_ms,
                task_trace_id,
                Some(failure),
            )
            .await;
        }

        // Resolve any escalations still pending for this task so the sweeper
        // doesn't auto-approve/deny them after the task has already failed.
        self.escalation_manager
            .resolve_for_task(&task.id, "Task failed")
            .await;

        self.cleanup_task_subscriptions(&task.id).await;

        // Free per-task kernel state. Failed tasks cannot resume — context window,
        // intent history, reject counters, and force-end flags are all dead state
        // and would otherwise leak when execute_task_sync bails before the
        // success-path cleanup at the end of the iteration loop.
        self.context_manager.remove_context(&task.id).await;
        self.intent_validator.remove_task(&task.id).await;
    }

    // ── Scratchpad auto-write ────────────────────────────────────────────────

    /// Generate and write a scratchpad note summarizing a completed or failed task.
    ///
    /// This is non-fatal: failures are logged as warnings and do not affect
    /// the task's outcome or downstream processing.
    async fn auto_write_scratchpad_note(&self, task: &AgentTask, outcome: &str) {
        if !self.config.scratchpad.enabled || !self.config.scratchpad.auto_write_on_completion {
            return;
        }

        // Fetch episodic timeline for this task
        let episodes = match self.episodic_memory.timeline_by_task(&task.id, 200).await {
            Ok(eps) => eps,
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "Failed to fetch episodic timeline for scratchpad auto-write"
                );
                return;
            }
        };

        // Skip trivial tasks
        if episodes.len() < self.config.scratchpad.auto_write_min_steps {
            tracing::debug!(
                task_id = %task.id,
                episode_count = episodes.len(),
                min_steps = self.config.scratchpad.auto_write_min_steps,
                "Skipping scratchpad auto-write for trivial task"
            );
            return;
        }

        // Get existing page titles for auto-linking and deduplication
        let agent_id_str = task.agent_id.as_uuid().to_string();
        let existing_pages = match self.scratchpad_store.list_pages(&agent_id_str).await {
            Ok(pages) => pages,
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "Failed to list scratchpad pages for auto-write"
                );
                return;
            }
        };
        let existing_titles: Vec<String> = existing_pages.iter().map(|p| p.title.clone()).collect();

        let summary = generate_task_summary(
            task,
            outcome,
            &episodes,
            &existing_titles,
            self.config.scratchpad.auto_write_max_summary,
        );

        // Deduplicate title
        let final_title = deduplicate_title(&summary.title, &existing_titles);

        match self
            .scratchpad_store
            .write_page(&agent_id_str, &final_title, &summary.content, &summary.tags)
            .await
        {
            Ok(_) => {
                tracing::info!(
                    agent_id = %task.agent_id,
                    title = %final_title,
                    "Auto-generated scratchpad note for completed task"
                );
            }
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "Failed to auto-write scratchpad note (non-fatal)"
                );
            }
        }
    }

    // ── Task-completion notification helpers ─────────────────────────────────

    /// Returns `true` only for root tasks (no parent) that the user sees directly.
    ///
    /// Checks both the legacy `parent_task` field (used by delegation) and the
    /// newer `parent_task_id` field (used by sub-agent spawning) so that neither
    /// delegation children nor sub-agent children receive user-visible notifications.
    pub(crate) fn is_root_task(task: &AgentTask) -> bool {
        task.parent_task.is_none() && task.parent_task_id.is_none()
    }

    /// Build the short subject line (≤80 chars) for a task-completion notification.
    fn format_completion_subject(outcome: TaskOutcome, prompt: &str) -> String {
        let (icon, verb) = match outcome {
            TaskOutcome::Success => ("✓", "completed"),
            TaskOutcome::Failed => ("✗", "failed"),
            TaskOutcome::Cancelled => ("○", "cancelled"),
            TaskOutcome::TimedOut => ("⏱", "timed out"),
        };
        let short_prompt: String = prompt.chars().take(50).collect();
        let ellipsis = if prompt.chars().count() > 50 {
            "…"
        } else {
            ""
        };
        format!("{icon} Task {verb}: {short_prompt}{ellipsis}")
    }

    /// Best-effort progress lookup for tasks that failed/timed out without a
    /// `TaskResult`. Reads episodic memory for the last `tool_call` entry and
    /// the latest checkpoint for the iteration counter. Returns
    /// `(last_tool, last_iteration, observed_iterations, observed_tool_calls)`.
    pub(crate) async fn gather_task_progress(
        &self,
        task_id: &TaskID,
    ) -> (Option<String>, Option<u32>, Option<u32>, Option<u32>) {
        let mut last_tool: Option<String> = None;
        let mut last_iteration: Option<u32> = None;
        let mut observed_tool_calls: Option<u32> = None;

        if let Ok(timeline) = self.episodic_memory.timeline_by_task(task_id, 200).await {
            let tool_call_entries: Vec<_> = timeline
                .iter()
                .filter(|e| e.entry_type == agentos_memory::EpisodeType::ToolCall)
                .collect();
            observed_tool_calls = Some(tool_call_entries.len() as u32);
            // Timeline is chronological; pick the most recent tool_call entry.
            if let Some(latest) = tool_call_entries.last() {
                if let Some(meta) = &latest.metadata {
                    last_tool = meta
                        .get("tool")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    // The executor writes the 0-based loop index to episodic
                    // metadata (see `task_executor.rs` `for iteration in
                    // 0..max_iterations`). The success-path metric reports
                    // `completed_iterations = iteration + 1` (1-based), and
                    // checkpoint `step_num` is also 1-based. Normalize to
                    // 1-based here so the user-visible "Last tool: foo
                    // (iteration N)" matches the "Iterations | M" metric.
                    last_iteration = meta
                        .get("iteration")
                        .and_then(|v| v.as_u64())
                        .map(|n| (n as u32).saturating_add(1));
                }
            }
        }

        // Both signals are 1-based and best-effort. They can disagree when a
        // checkpoint write succeeds but the episodic write for the same
        // iteration fails (or vice versa). Take the max so the user-visible
        // metric never under-reports progress relative to "Last tool: foo
        // (iteration N)".
        let observed_iterations = match (
            self.checkpoint_store
                .get_latest(task_id)
                .await
                .ok()
                .flatten()
                .map(|r| r.step_num),
            last_iteration,
        ) {
            (Some(cp), Some(li)) => Some(cp.max(li)),
            (Some(cp), None) => Some(cp),
            (None, li) => li,
        };

        (
            last_tool,
            last_iteration,
            observed_iterations,
            observed_tool_calls,
        )
    }

    /// Format a duration in milliseconds as `Hh Mm Ss` / `Mm Ss` / `Ss`.
    fn format_duration_human(duration_ms: u64) -> String {
        let total_s = duration_ms / 1000;
        let h = total_s / 3600;
        let m = (total_s % 3600) / 60;
        let s = total_s % 60;
        if h > 0 {
            format!("{h}h {m}m {s}s")
        } else if m > 0 {
            format!("{m}m {s}s")
        } else {
            format!("{}.{}s", total_s, (duration_ms % 1000) / 100)
        }
    }

    /// Render a UTC timestamp as `YYYY-MM-DD HH:MM:SS UTC`.
    fn format_utc(ts: DateTime<Utc>) -> String {
        ts.format("%Y-%m-%d %H:%M:%S UTC").to_string()
    }

    /// Truncate to `max` chars and append `…` if anything was dropped.
    fn truncate_chars_with_ellipsis(s: &str, max: usize) -> String {
        let count = s.chars().count();
        if count <= max {
            s.to_string()
        } else {
            let mut out: String = s.chars().take(max).collect();
            out.push('…');
            out
        }
    }

    /// Build the markdown body for a task-completion notification.
    ///
    /// On failure / timeout / cancellation this expands into a structured
    /// "WHY / WHERE / WHEN" report so the user can diagnose without opening
    /// logs. Success bodies stay terse.
    #[allow(clippy::too_many_arguments)]
    fn format_completion_body(
        outcome: TaskOutcome,
        task: &AgentTask,
        agent_name: &str,
        summary: &str,
        duration_ms: u64,
        iterations: Option<u32>,
        tool_calls: Option<u32>,
        cost_usd: Option<f64>,
        failure: Option<&FailureDetails>,
        finished_at: DateTime<Utc>,
    ) -> String {
        let duration_human = Self::format_duration_human(duration_ms);
        let iterations_str = iterations
            .map(|n| n.to_string())
            .unwrap_or_else(|| "N/A".to_string());
        let tool_calls_str = tool_calls
            .map(|n| n.to_string())
            .unwrap_or_else(|| "N/A".to_string());
        let cost_str = cost_usd
            .map(|c| format!("${c:.4} (period)"))
            .unwrap_or_else(|| "N/A".to_string());
        let prompt_preview = Self::truncate_chars_with_ellipsis(&task.original_prompt, 200);

        // Success path: keep the original compact layout.
        if matches!(outcome, TaskOutcome::Success) {
            return format!(
                "## Task {outcome}\n\n\
                **Original request:** {prompt_preview}\n\n\
                **Summary:** {summary}\n\n\
                | Metric | Value |\n\
                |--------|-------|\n\
                | Duration | {duration_human} |\n\
                | Iterations | {iterations_str} |\n\
                | Tool calls | {tool_calls_str} |\n\
                | Agent cost (period) | {cost_str} |\n",
            );
        }

        // Failure / timeout / cancelled — produce a structured report.
        let mut out = String::new();
        out.push_str(&format!("## Task {outcome} — {agent_name}\n\n"));

        // ── WHY ──────────────────────────────────────────────────────────
        out.push_str("### Why\n\n");
        if let Some(f) = failure {
            if !f.reason.is_empty() {
                out.push_str(&format!("**Reason:** `{}`\n\n", f.reason));
            }
            if let Some(root) = f.error_chain.first() {
                out.push_str(&format!(
                    "**Error:** {}\n\n",
                    Self::truncate_chars_with_ellipsis(root, 400)
                ));
            }
            if f.error_chain.len() > 1 {
                out.push_str("**Cause chain:**\n");
                for (i, e) in f.error_chain.iter().enumerate() {
                    out.push_str(&format!(
                        "{}. {}\n",
                        i + 1,
                        Self::truncate_chars_with_ellipsis(e, 240)
                    ));
                }
                out.push('\n');
            }
        } else {
            // Timeout / cancellation paths use `summary` as the explanation.
            out.push_str(&format!(
                "{}\n\n",
                Self::truncate_chars_with_ellipsis(summary, 400)
            ));
        }

        // ── WHERE ────────────────────────────────────────────────────────
        out.push_str("### Where\n\n");
        out.push_str(&format!("- **Agent:** {agent_name}\n"));
        out.push_str(&format!("- **Task:** `{}`\n", task.id));
        if let Some(f) = failure {
            match (&f.last_tool, f.last_iteration) {
                (Some(tool), Some(it)) => {
                    out.push_str(&format!("- **Last tool:** `{}` (iteration {})\n", tool, it));
                }
                (Some(tool), None) => {
                    out.push_str(&format!("- **Last tool:** `{}`\n", tool));
                }
                _ => {
                    out.push_str(
                        "- **Last tool:** _none — task failed before invoking any tool_\n",
                    );
                }
            }
        }
        out.push('\n');

        // ── WHEN ─────────────────────────────────────────────────────────
        out.push_str("### When\n\n");
        out.push_str(&format!(
            "- **Queued:** {}\n",
            Self::format_utc(task.created_at)
        ));
        if let Some(started) = task.started_at {
            let queue_wait_ms = (started - task.created_at).num_milliseconds().max(0) as u64;
            out.push_str(&format!(
                "- **Started:** {} (+{} queue wait)\n",
                Self::format_utc(started),
                Self::format_duration_human(queue_wait_ms)
            ));
        }
        out.push_str(&format!(
            "- **{}:** {}\n",
            match outcome {
                TaskOutcome::TimedOut => "Timed out",
                TaskOutcome::Cancelled => "Cancelled",
                _ => "Failed",
            },
            Self::format_utc(finished_at)
        ));
        out.push_str(&format!("- **Duration:** {duration_human}\n\n"));

        // ── METRICS ──────────────────────────────────────────────────────
        out.push_str("| Metric | Value |\n|--------|-------|\n");
        out.push_str(&format!("| Iterations | {iterations_str} |\n"));
        out.push_str(&format!("| Tool calls | {tool_calls_str} |\n"));
        out.push_str(&format!("| Agent cost (period) | {cost_str} |\n\n"));

        // ── REQUEST ──────────────────────────────────────────────────────
        // Use a fenced code block so the prompt renders as preformatted text
        // across all adapters. Telegram's HTML converter strips leading `>`
        // (it isn't recognised as a blockquote) and Slack/Discord render
        // fenced blocks consistently.
        //
        // A prompt containing a literal triple-backtick would close the fence
        // early and cause the rest of the notification to leak as raw markdown
        // (and on Telegram, the HTML render then fails with "unclosed entity").
        // Defuse by inserting a zero-width space between the backticks.
        let safe_prompt = prompt_preview.replace("```", "`\u{200B}``");
        out.push_str("### Original request\n\n");
        out.push_str("```\n");
        out.push_str(&safe_prompt);
        out.push_str("\n```\n");

        out
    }

    /// Deliver a `TaskComplete` `UserMessage` to the notification router.
    ///
    /// `tool_calls` and `iterations` are `None` when the data is not available
    /// (e.g. failure paths where `TaskResult` was never produced).
    ///
    /// `failure` carries structured WHY/WHERE diagnostics for non-success
    /// outcomes; pass `None` for `TaskOutcome::Success`.
    ///
    /// Non-fatal: logs a warning and continues if delivery fails.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send_completion_notification(
        &self,
        task: &AgentTask,
        outcome: TaskOutcome,
        summary: &str,
        tool_calls: Option<u32>,
        iterations: Option<u32>,
        duration_ms: u64,
        trace_id: TraceID,
        failure: Option<FailureDetails>,
    ) {
        debug_assert!(
            Self::is_root_task(task),
            "send_completion_notification called for sub-task"
        );

        let cost_usd = self
            .cost_tracker
            .get_snapshot(&task.agent_id)
            .await
            .map(|s| s.cost_usd);

        let agent_name = {
            let reg = self.agent_registry.read().await;
            reg.get_by_id(&task.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string())
        };
        let base_subject = Self::format_completion_subject(outcome, &task.original_prompt);
        let subject = format!("[{agent_name}] {base_subject}")
            .chars()
            .take(80)
            .collect::<String>();
        let body = Self::format_completion_body(
            outcome,
            task,
            &agent_name,
            summary,
            duration_ms,
            iterations,
            tool_calls,
            cost_usd,
            failure.as_ref(),
            chrono::Utc::now(),
        );
        let priority = match outcome {
            TaskOutcome::Success | TaskOutcome::Cancelled => NotificationPriority::Info,
            TaskOutcome::Failed | TaskOutcome::TimedOut => NotificationPriority::Warning,
        };

        let msg = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Kernel,
            task_id: Some(task.id),
            trace_id,
            kind: UserMessageKind::TaskComplete {
                task_id: task.id,
                outcome,
                summary: summary.chars().take(500).collect(),
                duration_ms,
                iterations: iterations.unwrap_or(0),
                cost_usd,
                tool_calls: tool_calls.unwrap_or(0),
            },
            priority,
            subject,
            body,
            interaction: None,
            delivery_status: HashMap::new(),
            response: None,
            created_at: chrono::Utc::now(),
            expires_at: None,
            read: false,
            thread_id: Some(task.id.to_string()),
            reply_to_external_id: None,
            attachment: None,
        };

        if let Err(e) = self.notification_router.deliver(msg).await {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "Failed to send task completion notification (non-fatal)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dummy_task(prompt: &str) -> AgentTask {
        AgentTask {
            original_prompt: prompt.to_string(),
            created_at: chrono::Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap(),
            started_at: Some(chrono::Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 2).unwrap()),
            ..AgentTask::default()
        }
    }

    #[test]
    fn truncate_empty_returns_empty() {
        assert_eq!(Kernel::truncate_chars_with_ellipsis("", 10), "");
    }

    #[test]
    fn truncate_under_max_unchanged() {
        assert_eq!(Kernel::truncate_chars_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_max_unchanged() {
        assert_eq!(Kernel::truncate_chars_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_over_max_appends_ellipsis() {
        assert_eq!(
            Kernel::truncate_chars_with_ellipsis("helloworld", 5),
            "hello…"
        );
    }

    #[test]
    fn truncate_unicode_safe() {
        // 4-byte emoji must not split mid-codepoint
        let s = "abc🦀🦀🦀def";
        assert_eq!(Kernel::truncate_chars_with_ellipsis(s, 5), "abc🦀🦀…");
        // exact-max with multi-byte chars
        assert_eq!(Kernel::truncate_chars_with_ellipsis("🦀🦀", 2), "🦀🦀");
    }

    #[test]
    fn format_duration_under_minute() {
        assert_eq!(Kernel::format_duration_human(0), "0.0s");
        assert_eq!(Kernel::format_duration_human(1500), "1.5s");
        assert_eq!(Kernel::format_duration_human(59_999), "59.9s");
    }

    #[test]
    fn format_duration_minutes_and_hours() {
        assert_eq!(Kernel::format_duration_human(60_000), "1m 0s");
        assert_eq!(Kernel::format_duration_human(125_000), "2m 5s");
        assert_eq!(Kernel::format_duration_human(3_600_000), "1h 0m 0s");
        assert_eq!(Kernel::format_duration_human(3_725_000), "1h 2m 5s");
    }

    #[test]
    fn failure_body_includes_why_where_when_sections() {
        let task = dummy_task("test prompt");
        let failure = FailureDetails {
            reason: "max_iterations".to_string(),
            error_chain: vec!["root cause".to_string(), "deeper cause".to_string()],
            last_tool: Some("web-search".to_string()),
            last_iteration: Some(7),
        };
        let now = chrono::Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 5).unwrap();
        let body = Kernel::format_completion_body(
            TaskOutcome::Failed,
            &task,
            "researcher",
            "ignored on failure",
            3000,
            Some(7),
            Some(4),
            Some(0.0123),
            Some(&failure),
            now,
        );
        assert!(body.contains("### Why"));
        assert!(body.contains("`max_iterations`"));
        assert!(body.contains("**Error:** root cause"));
        assert!(body.contains("**Cause chain:**"));
        assert!(body.contains("1. root cause"));
        assert!(body.contains("2. deeper cause"));
        assert!(body.contains("### Where"));
        assert!(body.contains("**Agent:** researcher"));
        assert!(body.contains("`web-search` (iteration 7)"));
        assert!(body.contains("### When"));
        assert!(body.contains("**Failed:**"));
        assert!(body.contains("**Duration:** 3.0s"));
        assert!(body.contains("Iterations | 7"));
        assert!(body.contains("Tool calls | 4"));
        assert!(body.contains("$0.0123"));
        // Original request must be fenced (not blockquoted) so Telegram renders it.
        assert!(body.contains("### Original request\n\n```\ntest prompt\n```"));
    }

    #[test]
    fn failure_body_truncates_long_prompt() {
        let long_prompt = "Z".repeat(500); // Z chosen to avoid clashing with body labels.
        let task = dummy_task(&long_prompt);
        let failure = FailureDetails::default();
        let now = chrono::Utc::now();
        let body = Kernel::format_completion_body(
            TaskOutcome::Failed,
            &task,
            "agent",
            "summary",
            1000,
            None,
            None,
            None,
            Some(&failure),
            now,
        );
        // Assert structurally — count-based assertions would silently break
        // the day someone adds a Z to any static body label.
        assert!(
            body.contains(&"Z".repeat(200)),
            "body should contain a run of 200 Zs"
        );
        assert!(
            !body.contains(&"Z".repeat(201)),
            "body must not contain 201 Zs (cap violated)"
        );
        let expected_block = format!("```\n{}…\n```", "Z".repeat(200));
        assert!(
            body.contains(&expected_block),
            "fenced block should contain truncated prompt with ellipsis"
        );
    }

    #[test]
    fn failure_body_handles_no_tool_calls() {
        let task = dummy_task("p");
        let failure = FailureDetails {
            reason: "llm_error".to_string(),
            error_chain: vec!["connection refused".to_string()],
            last_tool: None,
            last_iteration: None,
        };
        let body = Kernel::format_completion_body(
            TaskOutcome::Failed,
            &task,
            "a",
            "ignored",
            500,
            Some(0),
            Some(0),
            None,
            Some(&failure),
            chrono::Utc::now(),
        );
        assert!(body.contains("_none — task failed before invoking any tool_"));
    }

    #[test]
    fn success_body_keeps_compact_layout() {
        let task = dummy_task("a request");
        let body = Kernel::format_completion_body(
            TaskOutcome::Success,
            &task,
            "agent",
            "the answer",
            5000,
            Some(3),
            Some(2),
            Some(0.05),
            None,
            chrono::Utc::now(),
        );
        assert!(body.starts_with("## Task success"));
        assert!(body.contains("**Original request:** a request"));
        assert!(body.contains("**Summary:** the answer"));
        assert!(!body.contains("### Why"));
        assert!(!body.contains("### Where"));
    }

    #[test]
    fn failure_body_neutralizes_triple_backtick_in_prompt() {
        // A prompt that itself contains ``` must not close the fenced block early.
        let task = dummy_task("explain this code: ```rust\nfn x() {}\n```");
        let failure = FailureDetails::default();
        let body = Kernel::format_completion_body(
            TaskOutcome::Failed,
            &task,
            "agent",
            "summary",
            500,
            None,
            None,
            None,
            Some(&failure),
            chrono::Utc::now(),
        );
        // The opening + closing fences of the rendered block, plus zero
        // additional triple-backtick runs from the prompt body.
        let fence_runs = body.matches("```").count();
        assert_eq!(
            fence_runs, 2,
            "exactly one fenced block: opening + closing; embedded ``` must be neutralized"
        );
        // Zero-width space inserted between original backticks.
        assert!(body.contains("`\u{200B}``"));
    }

    #[test]
    fn timeout_body_uses_summary_when_failure_details_minimal() {
        let task = dummy_task("p");
        let failure = FailureDetails {
            reason: "timeout".to_string(),
            error_chain: vec!["Task timed out after 60s (limit 60s)".to_string()],
            last_tool: None,
            last_iteration: None,
        };
        let body = Kernel::format_completion_body(
            TaskOutcome::TimedOut,
            &task,
            "agent",
            "Task timed out after 60s (limit 60s)",
            60_000,
            None,
            None,
            None,
            Some(&failure),
            chrono::Utc::now(),
        );
        assert!(body.contains("**Timed out:**"));
        assert!(body.contains("Task timed out after 60s"));
    }

    #[test]
    fn is_root_task_rejects_legacy_and_new_parent_fields() {
        let mut t = dummy_task("p");
        assert!(Kernel::is_root_task(&t));
        t.parent_task_id = Some(TaskID::new());
        assert!(
            !Kernel::is_root_task(&t),
            "parent_task_id alone disqualifies"
        );
        t.parent_task_id = None;
        t.parent_task = Some(TaskID::new());
        assert!(!Kernel::is_root_task(&t), "parent_task alone disqualifies");
    }
}
