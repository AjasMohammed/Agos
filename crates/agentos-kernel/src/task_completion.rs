use crate::kernel::Kernel;
use crate::task_executor::TaskResult;
use agentos_types::*;

impl Kernel {
    /// Handle successful task completion: record to episodic memory, update scheduler state,
    /// emit events, notify background pool, wake dependency waiters, and trigger consolidation.
    pub(crate) async fn complete_task_success(
        &self,
        task: &AgentTask,
        result: &TaskResult,
        duration_ms: u64,
        task_trace_id: TraceID,
    ) {
        tracing::info!(
            "Task {} complete: {}",
            task.id,
            Self::truncate_for_prompt_payload(&result.answer, 100)
        );
        crate::metrics::record_task_completed(duration_ms, true);

        // Record enriched task success to episodic memory
        let summary_preview = format!(
            "Task: {}\nOutcome: Success\nTool calls: {}\nIterations: {}\nDuration: {}ms\nFinal answer preview: {}",
            task.original_prompt,
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

            // If this was a scheduled task, emit ScheduledTaskCompleted
            if let Some(schedule_id) = scheduled_job_id {
                if let Some(job) = self.schedule_manager.get_job(&schedule_id).await {
                    self.schedule_manager.emit_task_completed(&job).await;
                }
            }

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
                )
                .await;
                self.scheduler.requeue(waiter_id).await.ok();
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

        self.cleanup_task_subscriptions(&task.id).await;
    }

    /// Handle task failure: classify the error, record to episodic memory, update scheduler state,
    /// emit events, notify background pool, and clean up dependency edges.
    pub(crate) async fn complete_task_failure(
        &self,
        task: &AgentTask,
        error: anyhow::Error,
        duration_ms: u64,
        task_trace_id: TraceID,
    ) {
        let error_message = error.to_string();
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
            return;
        }

        tracing::error!("Task {} failed: {}", task.id, error_message);
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

        // If this was a scheduled task, emit ScheduledTaskFailed
        if let Some(schedule_id) = scheduled_job_id_on_failure {
            if let Some(job) = self.schedule_manager.get_job(&schedule_id).await {
                self.schedule_manager
                    .emit_task_failed(&job, &error_message)
                    .await;
            }
        }

        let failure_summary = format!(
            "Task failed: {}\nError: {}",
            task.original_prompt, error_message
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
                )
                .await;
            }
            Err(err) => {
                tracing::warn!(task_id = %task.id, error = %err, "Failed to record episodic memory");
            }
        }

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
            )
            .await;
            self.scheduler.requeue(waiter_id).await.ok();
        }

        self.cleanup_task_subscriptions(&task.id).await;
    }
}
