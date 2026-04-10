use crate::webhook_batcher::{format_webhook_context, BatchReady};
use agentos_types::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Consumes batched webhook events and creates agent tasks for processing.
///
/// This is the bridge between the webhook batcher (which accumulates and
/// debounces events) and the kernel's task scheduler.
pub struct WebhookWakeUp {
    kernel: Arc<crate::Kernel>,
    rx: mpsc::Receiver<BatchReady>,
    /// Maximum bytes of webhook payload to inject into agent context.
    max_context_bytes: usize,
}

impl WebhookWakeUp {
    pub fn new(
        kernel: Arc<crate::Kernel>,
        rx: mpsc::Receiver<BatchReady>,
        max_context_bytes: usize,
    ) -> Self {
        Self {
            kernel,
            rx,
            max_context_bytes,
        }
    }

    /// Run the wake-up loop until cancellation.
    pub async fn run(mut self, cancel: CancellationToken) {
        tracing::info!("Webhook wake-up loop started");
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("Webhook wake-up loop shutting down");
                    break;
                }
                batch = self.rx.recv() => {
                    match batch {
                        Some(batch) => self.handle_batch(batch).await,
                        None => {
                            tracing::info!("Webhook wake-up channel closed");
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn handle_batch(&self, batch: BatchReady) {
        let event_count = batch.events.len();
        let agent_id = batch.agent_id;
        let endpoint_id = batch.endpoint_id;
        let provider_name = batch.provider.to_string();

        // Format the webhook payload as a prompt for the agent
        let prompt = format_webhook_context(&batch, self.max_context_bytes);

        // Verify the agent is still connected
        let registry = self.kernel.agent_registry.read().await;
        let agent = match registry.get_by_id(&agent_id) {
            Some(a) if a.status != AgentStatus::Offline => a.clone(),
            _ => {
                tracing::warn!(
                    agent_id = %agent_id,
                    endpoint_id = %endpoint_id,
                    "Agent offline, dropping webhook batch ({event_count} events)",
                );
                return;
            }
        };

        let effective_permissions = registry.compute_effective_permissions(&agent_id);
        drop(registry);

        // Create a task for the agent
        let task_id = TaskID::new();
        let task_timeout =
            Duration::from_secs(self.kernel.config.kernel.autonomous_mode.task_timeout_secs);

        let capability_token = match self.kernel.capability_engine.issue_token(
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
            ]),
            effective_permissions,
            task_timeout,
        ) {
            Ok(token) => token,
            Err(e) => {
                tracing::error!(
                    agent_id = %agent_id,
                    error = %e,
                    "Failed to issue capability token for webhook task"
                );
                return;
            }
        };

        let task = AgentTask {
            id: task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token,
            assigned_llm: Some(agent.id),
            priority: 7, // slightly elevated — external events are time-sensitive
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: task_timeout,
            original_prompt: prompt,
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            max_iterations: None,
            trigger_source: None, // webhook-triggered, not event-subscription-triggered
            autonomous: true,
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
        };

        self.kernel.scheduler.enqueue(task).await;

        tracing::info!(
            task_id = %task_id,
            agent_id = %agent_id,
            endpoint_id = %endpoint_id,
            events = event_count,
            "Webhook batch spawned agent task"
        );

        // Emit event for audit/observability
        self.kernel
            .emit_event(
                EventType::WebhookReceived,
                EventSource::ExternalBridge,
                EventSeverity::Info,
                serde_json::json!({
                    "endpoint_id": endpoint_id.to_string(),
                    "agent_id": agent_id.to_string(),
                    "task_id": task_id.to_string(),
                    "event_count": event_count,
                    "provider": provider_name,
                }),
                0,
            )
            .await;
    }
}
