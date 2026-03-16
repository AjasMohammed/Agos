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
            }) {
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
            .unwrap_or(false);

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

            self.background_pool
                .complete(&task.id, serde_json::json!({ "result": result.answer }))
                .await;

            // Wake any parent tasks that were waiting on this child
            let waiters = self.scheduler.complete_dependency(task.id).await;
            for waiter_id in waiters {
                self.scheduler.requeue(&waiter_id).await.ok();
            }

            // Trigger consolidation bookkeeping in the background.
            let consolidation = self.consolidation_engine.clone();
            tokio::spawn(async move {
                consolidation.on_task_completed().await;
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
        let task_is_waiting = self
            .scheduler
            .get_task(&task.id)
            .await
            .map(|t| t.state == TaskState::Waiting)
            .unwrap_or(false);

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
            .unwrap_or(false);

        if !failed {
            tracing::info!(
                task_id = %task.id,
                "Task error but already in terminal state (likely timed out), skipping failure handling"
            );
            self.cleanup_task_subscriptions(&task.id).await;
            return;
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

        self.background_pool
            .fail(&task.id, error_message.clone())
            .await;

        let failure_summary =
            format!("Task failed: {}\nError: {}", task.original_prompt, error_message);
        match self
            .episodic_memory
            .record(agentos_memory::EpisodeRecordInput {
                task_id: &task.id,
                agent_id: &task.agent_id,
                entry_type: agentos_memory::EpisodeType::SystemEvent,
                content: &failure_summary,
                summary: Some("Task failed"),
                metadata: Some(
                    serde_json::json!({ "outcome": "failure", "error": error_message }),
                ),
                trace_id: &task_trace_id,
            }) {
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
        for waiter_id in waiters {
            self.scheduler.requeue(&waiter_id).await.ok();
        }

        self.cleanup_task_subscriptions(&task.id).await;
    }
}
