use crate::event_bus::{parse_event_type_filter, parse_subscription_priority};
use crate::injection_scanner::ThreatLevel;
use crate::kernel::Kernel;
use crate::tool_call::parse_tool_call;
use agentos_sandbox::SandboxConfig;
use agentos_sandbox::SandboxExecutor;
use agentos_tools::traits::ToolExecutionContext;
use agentos_types::*;
use std::sync::Arc;
use std::time::Duration;

/// Result of synchronous task execution, carrying data needed by the outer
/// `execute_task()` method for enriched episodic memory recording.
pub(crate) struct TaskResult {
    pub answer: String,
    pub tool_call_count: u32,
    pub iterations: u32,
}

impl Kernel {
    pub(crate) fn classify_task_failure(
        error_message: &str,
    ) -> (&'static str, EventSeverity, bool) {
        let lower = error_message.to_ascii_lowercase();
        if lower.starts_with("task paused:") {
            return ("task_paused", EventSeverity::Warning, true);
        }
        if lower.contains("llm error") {
            return ("llm_error", EventSeverity::Warning, false);
        }
        if lower.contains("budget") || lower.contains("wall-time") {
            return ("budget_exceeded", EventSeverity::Warning, false);
        }
        if lower.contains("max iterations") {
            return ("max_iterations", EventSeverity::Warning, false);
        }
        ("task_error", EventSeverity::Warning, false)
    }

    async fn register_task_subscription(&self, task_id: TaskID, subscription_id: SubscriptionID) {
        self.task_scoped_subscriptions
            .write()
            .await
            .entry(task_id)
            .or_default()
            .push(subscription_id);
    }

    async fn remove_task_subscription(&self, task_id: &TaskID, subscription_id: &SubscriptionID) {
        let mut scoped = self.task_scoped_subscriptions.write().await;
        if let Some(entries) = scoped.get_mut(task_id) {
            entries.retain(|id| id != subscription_id);
            if entries.is_empty() {
                scoped.remove(task_id);
            }
        }
    }

    pub(crate) async fn cleanup_task_subscriptions(&self, task_id: &TaskID) {
        let subs = self.task_scoped_subscriptions.write().await.remove(task_id);
        if let Some(sub_ids) = subs {
            for sub_id in sub_ids {
                self.event_bus.unsubscribe(&sub_id).await;
            }
        }
    }

    async fn schedule_subscription_removal(
        &self,
        subscription_id: SubscriptionID,
        duration: Duration,
    ) {
        let event_bus = self.event_bus.clone();
        let token = self.cancellation_token.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = tokio::time::sleep(duration) => {
                    event_bus.unsubscribe(&subscription_id).await;
                }
            }
        });
    }

    async fn handle_dynamic_event_subscription_intent(
        &self,
        task: &AgentTask,
        tool_call: &crate::tool_call::ParsedToolCall,
        trace_id: TraceID,
    ) -> Result<serde_json::Value, String> {
        if !task
            .capability_token
            .permissions
            .check("event.subscribe", PermissionOp::Write)
        {
            return Err("Missing required permission: event.subscribe (write)".to_string());
        }
        let now = chrono::Utc::now();
        let has_unexpired_grant = task
            .capability_token
            .permissions
            .entries
            .iter()
            .any(|entry| {
                (entry.resource == "event.subscribe"
                    || "event.subscribe".starts_with(&entry.resource))
                    && entry.write
                    && entry
                        .expires_at
                        .map(|expires| now <= expires)
                        .unwrap_or(true)
            });
        if !has_unexpired_grant {
            return Err("Permission denied: event.subscribe grant is expired".to_string());
        }

        match tool_call.intent_type {
            IntentType::Subscribe => {
                let payload: SubscribePayload =
                    serde_json::from_value(tool_call.payload.clone())
                        .map_err(|e| format!("Invalid subscribe payload: {}", e))?;

                let event_type_filter = parse_event_type_filter(&payload.event_filter)
                    .ok_or_else(|| {
                        format!(
                            "Invalid event filter '{}'. Use 'all', '*', 'category:<name>', '<Category>.*', or exact event names",
                            payload.event_filter
                        )
                    })?;

                let priority = parse_subscription_priority(payload.priority.as_deref())
                    .ok_or_else(|| {
                        "Invalid priority. Use 'critical', 'high', 'normal', or 'low'".to_string()
                    })?;

                // Validate TTL before creating the subscription to avoid orphaned entries.
                if let SubscriptionDuration::TTL { seconds } = &payload.duration {
                    if *seconds == 0 {
                        return Err("TTL seconds must be greater than 0".to_string());
                    }
                }

                let filter_predicate = payload.filter_predicate.and_then(|raw| {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                });

                let sub = EventSubscription {
                    id: SubscriptionID::new(),
                    agent_id: task.agent_id,
                    event_type_filter,
                    filter: filter_predicate.clone(),
                    priority,
                    throttle: ThrottlePolicy::None,
                    enabled: true,
                    created_at: chrono::Utc::now(),
                };

                let sub_id = self.event_bus.subscribe(sub).await;

                match payload.duration {
                    SubscriptionDuration::Task => {
                        self.register_task_subscription(task.id, sub_id).await;
                    }
                    SubscriptionDuration::Permanent => {}
                    SubscriptionDuration::TTL { seconds } => {
                        self.schedule_subscription_removal(sub_id, Duration::from_secs(seconds))
                            .await;
                    }
                }

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::EventSubscriptionCreated,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "subscription_id": sub_id.to_string(),
                        "event_filter": payload.event_filter,
                        "payload_filter": filter_predicate,
                        "duration": format!("{:?}", payload.duration),
                        "dynamic": true,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                Ok(serde_json::json!({
                    "subscription_id": sub_id.to_string(),
                    "status": "subscribed",
                }))
            }
            IntentType::Unsubscribe => {
                let payload: UnsubscribePayload = serde_json::from_value(tool_call.payload.clone())
                    .map_err(|e| format!("Invalid unsubscribe payload: {}", e))?;
                let sub_id = payload
                    .subscription_id
                    .parse::<SubscriptionID>()
                    .map_err(|_| format!("Invalid subscription ID: {}", payload.subscription_id))?;

                let sub = self
                    .event_bus
                    .get_subscription(&sub_id)
                    .await
                    .ok_or_else(|| format!("Subscription '{}' not found", sub_id))?;

                if sub.agent_id != task.agent_id {
                    return Err("Cannot unsubscribe another agent's subscription".to_string());
                }

                if !self.event_bus.unsubscribe(&sub_id).await {
                    return Err(format!("Subscription '{}' not found", sub_id));
                }
                self.remove_task_subscription(&task.id, &sub_id).await;

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::EventSubscriptionRemoved,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "subscription_id": sub_id.to_string(),
                        "dynamic": true,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                Ok(serde_json::json!({
                    "subscription_id": sub_id.to_string(),
                    "status": "unsubscribed",
                }))
            }
            _ => Err("Unsupported dynamic subscription intent".to_string()),
        }
    }

    pub(crate) async fn task_executor_loop(self: &Arc<Self>) {
        loop {
            tokio::select! {
                _ = self.cancellation_token.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if self.scheduler.running_count().await
                        >= self.config.kernel.max_concurrent_tasks
                    {
                        continue;
                    }
                    if let Some(task) = self.scheduler.dequeue().await {
                        let kernel = self.clone();
                        tokio::spawn(async move {
                            kernel.execute_task(&task).await;
                        });
                    }
                }
            }
        }
    }

    /// Validate a tool call against the capability token and permission system.
    pub(crate) fn validate_tool_call(
        &self,
        task: &AgentTask,
        tool_call: &crate::tool_call::ParsedToolCall,
        trace_id: TraceID,
    ) -> Result<(), String> {
        let intent = IntentMessage {
            id: MessageID::new(),
            sender_token: task.capability_token.clone(),
            intent_type: tool_call.intent_type,
            target: IntentTarget::Kernel,
            payload: SemanticPayload {
                schema: tool_call.tool_name.clone(),
                data: tool_call.payload.clone(),
            },
            context_ref: ContextID::new(),
            priority: task.priority,
            timeout_ms: task.timeout.as_millis() as u32,
            trace_id,
            timestamp: chrono::Utc::now(),
        };

        // Validate payload against registered JSON Schema (if any)
        self.schema_registry
            .validate(&tool_call.tool_name, &tool_call.payload)?;

        let required_perms = self
            .tool_runner
            .get_required_permissions(&tool_call.tool_name)
            .unwrap_or_default();

        let required_for_validate: Vec<(String, PermissionOp)> = required_perms;

        self.capability_engine
            .validate_intent(&task.capability_token, &intent, &required_for_validate)
            .map_err(|e| format!("{}", e))
    }

    /// Execute a single task synchronously: assemble context, call LLM, process tool calls, repeat.
    pub(crate) async fn execute_task_sync(
        &self,
        task: &AgentTask,
        task_trace_id: &TraceID,
    ) -> Result<TaskResult, anyhow::Error> {
        let agent = {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&task.agent_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", task.agent_id))?
        };

        let llm = {
            let active = self.active_llms.read().await;
            active.get(&agent.id).cloned()
        };

        let llm = match llm {
            Some(adapter) => adapter,
            None => {
                return Err(anyhow::anyhow!(
                    "LLM adapter for agent {} not connected",
                    agent.name
                ));
            }
        };

        // `current_llm` is mutable so it can be swapped when a model downgrade is triggered.
        let mut current_llm = llm;
        // Track whether we've already downgraded this task to avoid repeated swaps.
        let mut model_downgraded = false;

        // Setup task context: system prompt, context window, user prompt, injection scan,
        // and adaptive retrieval plan. Returns Err if task should be aborted.
        let (system_prompt, tools_desc, agent_directory, retrieval_plan) =
            self.setup_task_context(task, task_trace_id).await?;

        // 3. Agent loop: LLM → parse → tool call → push result → repeat
        let max_iterations = 10;
        let mut final_answer = String::new();
        let mut tool_call_count: u32 = 0;
        let mut completed_iterations: u32 = 0;
        let mut knowledge_blocks: Vec<String> = Vec::new();
        let mut refresh_knowledge_blocks = true;
        let mut context_warning_emitted = false;

        for iteration in 0..max_iterations {
            completed_iterations = iteration as u32 + 1;
            let iteration_trace_id = TraceID::new();
            let raw_context = match self.context_manager.get_context(&task.id).await {
                Ok(ctx) => ctx,
                Err(_) => break,
            };

            if refresh_knowledge_blocks {
                let refresh_start = std::time::Instant::now();
                knowledge_blocks.clear();
                if !retrieval_plan.is_empty() {
                    let retrieved = self
                        .retrieval_executor
                        .execute(&retrieval_plan, Some(&task.agent_id))
                        .await;
                    knowledge_blocks =
                        crate::retrieval_gate::RetrievalExecutor::format_as_knowledge_blocks(
                            &retrieved,
                        );
                    tracing::debug!(
                        task_id = %task.id,
                        iteration,
                        retrieval_queries = retrieval_plan.queries.len(),
                        retrieval_results = retrieved.len(),
                        retrieval_blocks = knowledge_blocks.len(),
                        "Adaptive retrieval complete"
                    );
                }
                if let Ok(blocks_context) = self.memory_blocks.blocks_for_context(&task.agent_id) {
                    if !blocks_context.is_empty() {
                        knowledge_blocks.push(format!(
                            "[AGENT_MEMORY_BLOCKS]\n{}\n[/AGENT_MEMORY_BLOCKS]",
                            blocks_context
                        ));
                    }
                }
                refresh_knowledge_blocks = false;
                crate::metrics::record_retrieval_refresh_decision(true);
                crate::metrics::record_retrieval_refresh(
                    refresh_start.elapsed().as_millis() as u64,
                    knowledge_blocks.len(),
                );
            } else {
                crate::metrics::record_retrieval_refresh_decision(false);
            }

            // Filter history: only non-system Active entries
            let history: Vec<ContextEntry> = raw_context
                .entries
                .into_iter()
                .filter(|e| {
                    e.role != ContextRole::System && e.partition == ContextPartition::Active
                })
                .collect();

            // Compile the optimized context window
            let compiled_context =
                self.context_compiler
                    .compile(crate::context_compiler::CompilationInputs {
                        system_prompt: system_prompt.clone(),
                        tool_descriptions: tools_desc.clone(),
                        agent_directory: agent_directory.clone(),
                        knowledge_blocks: knowledge_blocks.clone(),
                        history,
                        task_prompt: task.original_prompt.clone(),
                    });

            // --- Context window utilization check (Spec §7.4) ---
            // Emit ContextWindowNearLimit at most once per task when usage > 80%.
            if !context_warning_emitted {
                let estimated_tokens = compiled_context.estimated_tokens();
                let max_tokens = self.context_compiler.budget().usable_tokens();
                if max_tokens > 0 {
                    let utilization = estimated_tokens as f32 / max_tokens as f32;
                    if utilization > 0.80 {
                        let severity = if utilization > 0.95 {
                            EventSeverity::Critical
                        } else {
                            EventSeverity::Warning
                        };
                        let chain_depth = task
                            .trigger_source
                            .as_ref()
                            .map(|ts| ts.chain_depth + 1)
                            .unwrap_or(0);
                        self.emit_event_with_trace(
                            EventType::ContextWindowNearLimit,
                            EventSource::ContextManager,
                            severity,
                            serde_json::json!({
                                "task_id": task.id.to_string(),
                                "agent_id": task.agent_id.to_string(),
                                "estimated_tokens": estimated_tokens,
                                "max_tokens": max_tokens,
                                "utilization_percent": (utilization * 100.0) as u32,
                            }),
                            chain_depth,
                            Some(iteration_trace_id),
                        )
                        .await;
                        context_warning_emitted = true;
                    }
                }
            }

            // --- Model allowlist check (Spec §4) ---
            // Reject inference calls to models not in the agent's allowlist.
            let model_check = self
                .cost_tracker
                .validate_model(&task.agent_id, current_llm.model_name())
                .await;
            if let crate::cost_tracker::BudgetCheckResult::ModelNotAllowed { model, agent_id: _ } =
                &model_check
            {
                tracing::error!(
                    "Task {} agent {} model '{}' not in allowlist — inference denied",
                    task.id,
                    task.agent_id,
                    model
                );
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: iteration_trace_id,
                    event_type: agentos_audit::AuditEventType::PermissionDenied,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "model": model,
                        "reason": "model_not_in_allowlist",
                    }),
                    severity: agentos_audit::AuditSeverity::Security,
                    reversible: false,
                    rollback_ref: None,
                });
                self.context_manager.remove_context(&task.id).await;
                self.intent_validator.remove_task(&task.id).await;
                anyhow::bail!("Model '{}' not in agent's allowed models list", model);
            }

            // --- Pre-inference budget check (Spec §4) ---
            // Check BEFORE consuming tokens so we don't waste an inference call on a
            // budget that is already exhausted.
            let pre_check = self.cost_tracker.check_budget(&task.agent_id).await;
            if let crate::cost_tracker::BudgetCheckResult::HardLimitExceeded { resource, action } =
                pre_check
            {
                tracing::error!(
                    "Task {} pre-inference budget EXCEEDED for {}: action {:?} — skipping LLM call",
                    task.id,
                    resource,
                    action
                );
                // Checkpoint state before suspending so the task can be resumed
                self.take_snapshot(&task.id, "pre_inference_budget_exceeded", None)
                    .await;
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: iteration_trace_id,
                    event_type: agentos_audit::AuditEventType::BudgetExceeded,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "resource": resource,
                        "action": format!("{:?}", action),
                        "phase": "pre_inference",
                    }),
                    severity: agentos_audit::AuditSeverity::Security,
                    reversible: false,
                    rollback_ref: None,
                });
                self.context_manager.remove_context(&task.id).await;
                self.intent_validator.remove_task(&task.id).await;
                anyhow::bail!(
                    "Budget hard limit exceeded (pre-inference check): {}",
                    resource
                );
            }

            tracing::info!("Task {} iteration {}: calling LLM", task.id, iteration);

            let inference = match current_llm.infer(&compiled_context).await {
                Ok(mut result) => {
                    // Parse uncertainty declarations from the LLM response
                    if result.uncertainty.is_none() {
                        result.uncertainty = agentos_llm::parse_uncertainty(&result.text);
                    }
                    result
                }
                Err(e) => {
                    self.context_manager.remove_context(&task.id).await;
                    anyhow::bail!("LLM error: {}", e);
                }
            };

            crate::metrics::record_inference(
                current_llm.provider_name(),
                current_llm.model_name(),
                inference.tokens_used.prompt_tokens,
                inference.tokens_used.completion_tokens,
                inference.duration_ms,
            );
            tracing::info!(
                "Task {} LLM responded ({} tokens, {}ms)",
                task.id,
                inference.tokens_used.total_tokens,
                inference.duration_ms
            );

            // --- Cost budget enforcement ---
            let budget_result = self
                .cost_tracker
                .record_inference(
                    &task.agent_id,
                    &inference.tokens_used,
                    current_llm.provider_name(),
                    current_llm.model_name(),
                )
                .await;

            // --- Structured cost attribution audit entry (Spec §4) ---
            if let Some(snapshot) = self.cost_tracker.get_snapshot(&task.agent_id).await {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: iteration_trace_id,
                    event_type: agentos_audit::AuditEventType::CostAttribution,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "model": current_llm.model_name(),
                        "provider": current_llm.provider_name(),
                        "input_tokens": inference.tokens_used.prompt_tokens,
                        "output_tokens": inference.tokens_used.completion_tokens,
                        "tool_calls": snapshot.tool_calls,
                        "cost_usd": snapshot.cost_usd,
                        "cumulative_today_usd": snapshot.cost_usd,
                        "budget_remaining_usd": (snapshot.budget.max_cost_usd_per_day - snapshot.cost_usd).max(0.0),
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
            }

            match &budget_result {
                crate::cost_tracker::BudgetCheckResult::Warning {
                    resource,
                    current_pct,
                } => {
                    tracing::warn!(
                        "Task {} agent {} budget warning: {} at {:.1}%",
                        task.id,
                        task.agent_id,
                        resource,
                        current_pct
                    );
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: iteration_trace_id,
                        event_type: agentos_audit::AuditEventType::BudgetWarning,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "resource": resource,
                            "current_pct": current_pct,
                        }),
                        severity: agentos_audit::AuditSeverity::Warn,
                        reversible: false,
                        rollback_ref: None,
                    });
                }
                crate::cost_tracker::BudgetCheckResult::PauseRequired {
                    resource,
                    current_pct,
                } => {
                    tracing::warn!(
                        "Task {} agent {} budget pause: {} at {:.1}%",
                        task.id,
                        task.agent_id,
                        resource,
                        current_pct
                    );
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: iteration_trace_id,
                        event_type: agentos_audit::AuditEventType::BudgetExceeded,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "resource": resource,
                            "current_pct": current_pct,
                            "action": "pause",
                        }),
                        severity: agentos_audit::AuditSeverity::Warn,
                        reversible: false,
                        rollback_ref: None,
                    });
                    self.context_manager.remove_context(&task.id).await;
                    self.intent_validator.remove_task(&task.id).await;
                    anyhow::bail!(
                        "Budget pause threshold reached: {} at {:.1}%",
                        resource,
                        current_pct
                    );
                }
                crate::cost_tracker::BudgetCheckResult::HardLimitExceeded { resource, action } => {
                    tracing::error!(
                        "Task {} agent {} budget EXCEEDED: {} — action: {:?}",
                        task.id,
                        task.agent_id,
                        resource,
                        action
                    );
                    // Checkpoint before suspension so state is not lost (Spec §4/#5)
                    self.take_snapshot(&task.id, "post_inference_budget_exceeded", None)
                        .await;
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: iteration_trace_id,
                        event_type: agentos_audit::AuditEventType::BudgetExceeded,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "resource": resource,
                            "action": format!("{:?}", action),
                            "phase": "post_inference",
                        }),
                        severity: agentos_audit::AuditSeverity::Security,
                        reversible: false,
                        rollback_ref: None,
                    });
                    self.context_manager.remove_context(&task.id).await;
                    self.intent_validator.remove_task(&task.id).await;
                    anyhow::bail!("Budget hard limit exceeded: {}", resource);
                }
                crate::cost_tracker::BudgetCheckResult::ModelDowngradeRecommended {
                    downgrade_to,
                    provider,
                    resource,
                    current_pct,
                } => {
                    if !model_downgraded {
                        tracing::warn!(
                            "Task {} agent {} budget at {:.1}% for {} — downgrading model to {}/{}",
                            task.id,
                            task.agent_id,
                            current_pct,
                            resource,
                            provider,
                            downgrade_to
                        );
                        self.audit_log(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: iteration_trace_id,
                            event_type: agentos_audit::AuditEventType::BudgetWarning,
                            agent_id: Some(task.agent_id),
                            task_id: Some(task.id),
                            tool_id: None,
                            details: serde_json::json!({
                                "resource": resource,
                                "current_pct": current_pct,
                                "action": "model_downgrade",
                                "downgrade_to": downgrade_to,
                                "provider": provider,
                            }),
                            severity: agentos_audit::AuditSeverity::Warn,
                            reversible: false,
                            rollback_ref: None,
                        });

                        // Attempt to find an LLM for the downgrade model across all agents
                        let downgrade_llm = {
                            let active = self.active_llms.read().await;
                            active
                                .values()
                                .find(|llm| {
                                    llm.model_name() == downgrade_to.as_str()
                                        && llm.provider_name() == provider.as_str()
                                })
                                .cloned()
                        };

                        if let Some(cheaper_llm) = downgrade_llm {
                            tracing::info!(
                                "Task {} switching to downgrade model {}/{} for remaining iterations",
                                task.id, provider, downgrade_to
                            );
                            current_llm = cheaper_llm;
                            model_downgraded = true;
                        } else {
                            tracing::warn!(
                                "Task {} downgrade model {}/{} not available — falling through to PauseRequired",
                                task.id, provider, downgrade_to
                            );
                            self.context_manager.remove_context(&task.id).await;
                            self.intent_validator.remove_task(&task.id).await;
                            anyhow::bail!("Budget pause threshold reached: {} at {:.1}% (downgrade model unavailable)", resource, current_pct);
                        }
                    }
                    // If already downgraded, continue silently — we are already on the cheaper model
                }
                crate::cost_tracker::BudgetCheckResult::Ok => {}
                crate::cost_tracker::BudgetCheckResult::ModelNotAllowed { .. } => {
                    // Already handled by the explicit model check above; unreachable here.
                }
                crate::cost_tracker::BudgetCheckResult::WallTimeExceeded {
                    elapsed_secs,
                    limit_secs,
                } => {
                    tracing::error!(
                        "Task {} agent {} wall-time exceeded: {}s / {}s limit",
                        task.id,
                        task.agent_id,
                        elapsed_secs,
                        limit_secs
                    );
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: iteration_trace_id,
                        event_type: agentos_audit::AuditEventType::BudgetExceeded,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "resource": "wall_time",
                            "elapsed_secs": elapsed_secs,
                            "limit_secs": limit_secs,
                        }),
                        severity: agentos_audit::AuditSeverity::Warn,
                        reversible: false,
                        rollback_ref: None,
                    });
                    self.context_manager.remove_context(&task.id).await;
                    self.intent_validator.remove_task(&task.id).await;
                    anyhow::bail!(
                        "Wall-time exceeded: {}s / {}s limit",
                        elapsed_secs,
                        limit_secs
                    );
                }
            }

            // Push assistant response into context
            self.context_manager
                .push_entry(
                    &task.id,
                    ContextEntry {
                        role: ContextRole::Assistant,
                        content: inference.text.clone(),
                        timestamp: chrono::Utc::now(),
                        metadata: None,
                        importance: 0.4,
                        pinned: false,
                        reference_count: 0,
                        partition: ContextPartition::default(),
                        category: ContextCategory::History,
                    },
                )
                .await
                .ok();

            if let Err(e) = self
                .episodic_memory
                .record(agentos_memory::EpisodeRecordInput {
                    task_id: &task.id,
                    agent_id: &task.agent_id,
                    entry_type: agentos_memory::EpisodeType::LLMResponse,
                    content: &inference.text,
                    summary: Some(&format!(
                        "LLM response ({} tokens)",
                        inference.tokens_used.total_tokens
                    )),
                    metadata: None,
                    trace_id: &iteration_trace_id,
                })
            {
                tracing::warn!(task_id = %task.id, error = %e, "Failed to record episodic memory");
            }

            // Check for tool call
            match parse_tool_call(&inference.text) {
                Some(tool_call) => {
                    tracing::info!(
                        "Task {} tool call: {} ({:?})",
                        task.id,
                        tool_call.tool_name,
                        tool_call.intent_type
                    );

                    let trace_id = TraceID::new();

                    if matches!(
                        tool_call.intent_type,
                        IntentType::Subscribe | IntentType::Unsubscribe
                    ) {
                        match self
                            .validate_tool_call_full(task, &tool_call, trace_id)
                            .await
                        {
                            Err(denial_reason) => {
                                let error_result = serde_json::json!({
                                    "error": format!("Permission denied: {}", denial_reason)
                                });
                                self.context_manager
                                    .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                    .await
                                    .ok();
                                continue;
                            }
                            Ok(IntentCoherenceResult::Rejected { reason }) => {
                                let error_result = serde_json::json!({
                                    "error": format!("Coherence check failed: {}", reason)
                                });
                                self.context_manager
                                    .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                    .await
                                    .ok();
                                continue;
                            }
                            Ok(
                                IntentCoherenceResult::Suspicious { .. }
                                | IntentCoherenceResult::Approved,
                            ) => {}
                        }

                        self.intent_validator
                            .record_tool_call(&task.id, &tool_call)
                            .await;
                        tool_call_count += 1;
                        let dynamic_result = self
                            .handle_dynamic_event_subscription_intent(task, &tool_call, trace_id)
                            .await;
                        let context_result = match dynamic_result {
                            Ok(value) => value,
                            Err(err) => serde_json::json!({ "error": err }),
                        };
                        self.context_manager
                            .push_tool_result(&task.id, &tool_call.tool_name, &context_result)
                            .await
                            .ok();
                        continue;
                    }

                    enum ToolAccessCheck {
                        Unauthorized { allowed_tool_names: Vec<String> },
                        UnknownTool,
                    }
                    let tool_access_check = {
                        let registry = self.tool_registry.read().await;
                        let requested_tool = registry.get_by_name(&tool_call.tool_name);
                        if requested_tool.is_none() {
                            Some(ToolAccessCheck::UnknownTool)
                        } else if task.capability_token.allowed_tools.is_empty() {
                            // Empty allowed_tools means unrestricted by tool ID;
                            // permission checks are enforced in validate_tool_call_full.
                            None
                        } else {
                            let requested_tool_id = requested_tool.map(|tool| tool.id);
                            if requested_tool_id
                                .map(|id| !task.capability_token.allowed_tools.contains(&id))
                                .unwrap_or(true)
                            {
                                let allowed_tool_names = task
                                    .capability_token
                                    .allowed_tools
                                    .iter()
                                    .map(|tool_id| {
                                        registry
                                            .get_by_id(tool_id)
                                            .map(|tool| tool.manifest.manifest.name.clone())
                                            .unwrap_or_else(|| tool_id.to_string())
                                    })
                                    .collect::<Vec<_>>();
                                Some(ToolAccessCheck::Unauthorized { allowed_tool_names })
                            } else {
                                None
                            }
                        }
                    };
                    if let Some(tool_access_check) = tool_access_check {
                        match tool_access_check {
                            ToolAccessCheck::UnknownTool => {
                                self.audit_log(agentos_audit::AuditEntry {
                                    timestamp: chrono::Utc::now(),
                                    trace_id,
                                    event_type: agentos_audit::AuditEventType::PermissionDenied,
                                    agent_id: Some(task.agent_id),
                                    task_id: Some(task.id),
                                    tool_id: None,
                                    details: serde_json::json!({
                                        "tool": tool_call.tool_name,
                                        "reason": "tool_not_registered",
                                    }),
                                    severity: agentos_audit::AuditSeverity::Security,
                                    reversible: false,
                                    rollback_ref: None,
                                });
                                let chain_depth = task
                                    .trigger_source
                                    .as_ref()
                                    .map(|ts| ts.chain_depth + 1)
                                    .unwrap_or(0);
                                self.emit_event_with_trace(
                                    EventType::UnauthorizedToolAccess,
                                    EventSource::SecurityEngine,
                                    EventSeverity::Warning,
                                    serde_json::json!({
                                        "task_id": task.id.to_string(),
                                        "agent_id": task.agent_id.to_string(),
                                        "requested_tool": tool_call.tool_name,
                                        "agent_allowed_tools": [],
                                        "failure_reason": "tool_not_registered",
                                        "action_taken": "blocked",
                                    }),
                                    chain_depth,
                                    Some(trace_id),
                                )
                                .await;

                                let error_result = serde_json::json!({
                                    "error": format!("Unknown tool requested: {}", tool_call.tool_name)
                                });
                                self.context_manager
                                    .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                    .await
                                    .ok();
                            }
                            ToolAccessCheck::Unauthorized { allowed_tool_names } => {
                                self.audit_log(agentos_audit::AuditEntry {
                                    timestamp: chrono::Utc::now(),
                                    trace_id,
                                    event_type: agentos_audit::AuditEventType::PermissionDenied,
                                    agent_id: Some(task.agent_id),
                                    task_id: Some(task.id),
                                    tool_id: None,
                                    details: serde_json::json!({
                                        "tool": tool_call.tool_name,
                                        "reason": "tool_not_allowed_by_capability_token",
                                        "agent_allowed_tools": allowed_tool_names.clone(),
                                    }),
                                    severity: agentos_audit::AuditSeverity::Security,
                                    reversible: false,
                                    rollback_ref: None,
                                });
                                let chain_depth = task
                                    .trigger_source
                                    .as_ref()
                                    .map(|ts| ts.chain_depth + 1)
                                    .unwrap_or(0);
                                self.emit_event_with_trace(
                                    EventType::UnauthorizedToolAccess,
                                    EventSource::SecurityEngine,
                                    EventSeverity::Critical,
                                    serde_json::json!({
                                        "task_id": task.id.to_string(),
                                        "agent_id": task.agent_id.to_string(),
                                        "requested_tool": tool_call.tool_name,
                                        "agent_allowed_tools": allowed_tool_names,
                                        "failure_reason": "tool_not_allowed_by_capability_token",
                                        "action_taken": "blocked",
                                    }),
                                    chain_depth,
                                    Some(trace_id),
                                )
                                .await;

                                let error_result = serde_json::json!({
                                    "error": format!("Unauthorized tool access blocked: {}", tool_call.tool_name)
                                });
                                self.context_manager
                                    .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                    .await
                                    .ok();
                            }
                        }
                        continue;
                    }

                    // Full validation: structural (capability/schema) + semantic coherence
                    match self
                        .validate_tool_call_full(task, &tool_call, trace_id)
                        .await
                    {
                        Err(denial_reason) => {
                            tracing::warn!(
                                "Task {} permission denied for tool {}: {}",
                                task.id,
                                tool_call.tool_name,
                                denial_reason
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::PermissionDenied,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({
                                    "tool": tool_call.tool_name,
                                    "intent_type": format!("{:?}", tool_call.intent_type),
                                    "reason": denial_reason,
                                }),
                                severity: agentos_audit::AuditSeverity::Security,
                                reversible: false,
                                rollback_ref: None,
                            });

                            let required_permissions = self
                                .tool_runner
                                .get_required_permissions(&tool_call.tool_name)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|(resource, op)| format!("{}:{:?}", resource, op))
                                .collect::<Vec<_>>();
                            let chain_depth = task
                                .trigger_source
                                .as_ref()
                                .map(|ts| ts.chain_depth + 1)
                                .unwrap_or(0);
                            self.emit_event_with_trace(
                                EventType::CapabilityViolation,
                                EventSource::SecurityEngine,
                                EventSeverity::Critical,
                                serde_json::json!({
                                    "task_id": task.id.to_string(),
                                    "agent_id": task.agent_id.to_string(),
                                    "tool_name": tool_call.tool_name,
                                    "required_permissions": required_permissions,
                                    "violation_reason": denial_reason,
                                    "action_taken": "blocked",
                                }),
                                chain_depth,
                                Some(trace_id),
                            )
                            .await;

                            let error_result = serde_json::json!({
                                "error": format!("Permission denied: {}", denial_reason)
                            });
                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                .await
                                .ok();
                            continue;
                        }
                        Ok(IntentCoherenceResult::Rejected { reason }) => {
                            tracing::warn!(
                                "Task {} coherence rejected for tool {}: {}",
                                task.id,
                                tool_call.tool_name,
                                reason
                            );
                            let error_result = serde_json::json!({
                                "error": format!("Coherence check failed: {}", reason)
                            });
                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                .await
                                .ok();
                            continue;
                        }
                        Ok(
                            IntentCoherenceResult::Suspicious { .. }
                            | IntentCoherenceResult::Approved,
                        ) => {
                            // Suspicious: logged by validate_tool_call_full, proceed for now
                            // Approved: all clear
                        }
                    }

                    // Record this tool call for future coherence checks
                    self.intent_validator
                        .record_tool_call(&task.id, &tool_call)
                        .await;
                    tool_call_count += 1;

                    // Check tool call budget
                    let tool_budget = self.cost_tracker.record_tool_call(&task.agent_id).await;
                    if let crate::cost_tracker::BudgetCheckResult::HardLimitExceeded {
                        resource,
                        action,
                    } = &tool_budget
                    {
                        tracing::error!(
                            "Task {} agent {} tool call budget EXCEEDED: {} — action: {:?}",
                            task.id,
                            task.agent_id,
                            resource,
                            action
                        );
                        self.audit_log(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id,
                            event_type: agentos_audit::AuditEventType::BudgetExceeded,
                            agent_id: Some(task.agent_id),
                            task_id: Some(task.id),
                            tool_id: None,
                            details: serde_json::json!({
                                "resource": resource,
                                "action": format!("{:?}", action),
                            }),
                            severity: agentos_audit::AuditSeverity::Security,
                            reversible: false,
                            rollback_ref: None,
                        });
                        self.context_manager.remove_context(&task.id).await;
                        self.intent_validator.remove_task(&task.id).await;
                        anyhow::bail!("Tool call budget exceeded");
                    }

                    // --- Risk classification gate ---
                    let resource_hint = tool_call
                        .payload
                        .get("path")
                        .or_else(|| tool_call.payload.get("target"))
                        .or_else(|| tool_call.payload.get("file"))
                        .and_then(|v| v.as_str());
                    let risk_level = self.risk_classifier.classify(
                        tool_call.intent_type,
                        &tool_call.tool_name,
                        resource_hint,
                    );

                    match risk_level {
                        ActionRiskLevel::Forbidden => {
                            tracing::error!(
                                "Task {} tool '{}' FORBIDDEN by risk classifier",
                                task.id,
                                tool_call.tool_name
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::ActionForbidden,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({
                                    "tool": tool_call.tool_name,
                                    "intent_type": format!("{:?}", tool_call.intent_type),
                                    "resource": resource_hint,
                                    "risk_level": "Forbidden",
                                }),
                                severity: agentos_audit::AuditSeverity::Security,
                                reversible: false,
                                rollback_ref: None,
                            });
                            let error_result = serde_json::json!({
                                "error": "Action forbidden by security policy"
                            });
                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                .await
                                .ok();
                            continue;
                        }
                        ActionRiskLevel::HardApproval => {
                            tracing::warn!(
                                "Task {} tool '{}' requires hard approval — creating escalation",
                                task.id,
                                tool_call.tool_name
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::RiskEscalation,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({
                                    "tool": tool_call.tool_name,
                                    "intent_type": format!("{:?}", tool_call.intent_type),
                                    "resource": resource_hint,
                                    "risk_level": "HardApproval",
                                }),
                                severity: agentos_audit::AuditSeverity::Security,
                                reversible: false,
                                rollback_ref: None,
                            });
                            self.escalation_manager
                                .create_escalation(
                                    task.id,
                                    task.agent_id,
                                    crate::kernel_action::EscalationReason::AuthorizationRequired,
                                    format!(
                                        "Tool '{}' classified as high-risk (HardApproval). Resource: {:?}",
                                        tool_call.tool_name, resource_hint
                                    ),
                                    format!(
                                        "Allow agent to execute '{}' with intent {:?}?",
                                        tool_call.tool_name, tool_call.intent_type
                                    ),
                                    vec!["Approve".to_string(), "Deny".to_string()],
                                    "high".to_string(),
                                    true,
                                    trace_id,
                                    None, // auto_action: default deny on expiry
                                )
                                .await;
                            self.scheduler
                                .update_state(&task.id, TaskState::Waiting)
                                .await
                                .ok();
                            let waiting_result = serde_json::json!({
                                "status": "awaiting_approval",
                                "message": "This action requires human approval. Task is paused."
                            });
                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &waiting_result)
                                .await
                                .ok();
                            self.context_manager.remove_context(&task.id).await;
                            self.intent_validator.remove_task(&task.id).await;
                            anyhow::bail!(
                                "Task paused: tool '{}' requires hard approval",
                                tool_call.tool_name
                            );
                        }
                        ActionRiskLevel::SoftApproval => {
                            tracing::info!(
                                "Task {} tool '{}' classified as SoftApproval — logging and proceeding",
                                task.id, tool_call.tool_name
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::RiskEscalation,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({
                                    "tool": tool_call.tool_name,
                                    "intent_type": format!("{:?}", tool_call.intent_type),
                                    "resource": resource_hint,
                                    "risk_level": "SoftApproval",
                                }),
                                severity: agentos_audit::AuditSeverity::Warn,
                                reversible: false,
                                rollback_ref: None,
                            });
                            // Create a non-blocking escalation for visibility
                            self.escalation_manager
                                .create_escalation(
                                    task.id,
                                    task.agent_id,
                                    crate::kernel_action::EscalationReason::AuthorizationRequired,
                                    format!(
                                        "Tool '{}' classified as moderate-risk (SoftApproval). Resource: {:?}",
                                        tool_call.tool_name, resource_hint
                                    ),
                                    format!(
                                        "Agent is executing '{}' — cancel within review window if needed",
                                        tool_call.tool_name
                                    ),
                                    vec!["Acknowledge".to_string(), "Cancel".to_string()],
                                    "normal".to_string(),
                                    false, // non-blocking
                                    trace_id,
                                    Some(crate::escalation::AutoAction::Approve), // soft-approval
                                )
                                .await;
                        }
                        ActionRiskLevel::Notify => {
                            tracing::debug!(
                                "Task {} tool '{}' classified as Notify",
                                task.id,
                                tool_call.tool_name
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::RiskEscalation,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({
                                    "tool": tool_call.tool_name,
                                    "risk_level": "Notify",
                                }),
                                severity: agentos_audit::AuditSeverity::Info,
                                reversible: false,
                                rollback_ref: None,
                            });
                        }
                        ActionRiskLevel::Autonomous => {
                            // No action needed — proceed silently
                        }
                    }

                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id,
                        event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({ "tool": tool_call.tool_name }),
                        severity: agentos_audit::AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    });

                    if let Err(e) =
                        self.episodic_memory
                            .record(agentos_memory::EpisodeRecordInput {
                                task_id: &task.id,
                                agent_id: &task.agent_id,
                                entry_type: agentos_memory::EpisodeType::ToolCall,
                                content: &format!(
                                    "Tool: {} Payload: {}",
                                    tool_call.tool_name, tool_call.payload
                                ),
                                summary: Some(&format!(
                                    "Called tool: {} ({:?})",
                                    tool_call.tool_name, tool_call.intent_type
                                )),
                                metadata: Some(serde_json::json!({
                                    "tool": tool_call.tool_name,
                                    "intent_type": format!("{:?}", tool_call.intent_type),
                                    "iteration": iteration,
                                })),
                                trace_id: &trace_id,
                            })
                    {
                        tracing::warn!(task_id = %task.id, error = %e, "Failed to record episodic memory");
                    }

                    // --- Checkpoint before reversible (write) operations (Spec §5) ---
                    let snapshot_ref = if tool_call.intent_type == IntentType::Write
                        || tool_call.intent_type == IntentType::Execute
                    {
                        self.take_snapshot(&task.id, &tool_call.tool_name, Some(&tool_call.payload))
                            .await
                    } else {
                        None
                    };

                    let exec_context = ToolExecutionContext {
                        data_dir: self.data_dir.clone(),
                        task_id: task.id,
                        agent_id: task.agent_id,
                        trace_id,
                        permissions: task.capability_token.permissions.clone(),
                        vault: Some(std::sync::Arc::new(agentos_vault::ProxyVault::new(
                            self.vault.clone(),
                        ))),
                        hal: Some(self.hal.clone()),
                        // ToolRunner::execute() always overrides this with the shared registry.
                        file_lock_registry: None,
                    };
                    let tool_payload_preview = Self::truncate_for_prompt_payload(
                        &serde_json::to_string(&tool_call.payload).unwrap_or_default(),
                        600,
                    );

                    let tool_start = std::time::Instant::now();
                    let tool_result = {
                        let sandbox_config = {
                            let registry = self.tool_registry.read().await;
                            registry
                                .get_by_name(&tool_call.tool_name)
                                .map(|t| SandboxConfig::from_manifest(&t.manifest.sandbox))
                        };

                        if let Some(config) = sandbox_config {
                            let timeout = Duration::from_millis(config.max_cpu_ms.max(5000));
                            match self
                                .sandbox
                                .spawn(
                                    &tool_call.tool_name,
                                    tool_call.payload.clone(),
                                    &config,
                                    timeout,
                                )
                                .await
                            {
                                Ok(sandbox_result) => {
                                    SandboxExecutor::parse_result(&sandbox_result)
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        tool = %tool_call.tool_name,
                                        error = %e,
                                        "Sandbox spawn failed, falling back to in-process execution"
                                    );
                                    self.tool_runner
                                        .execute(
                                            &tool_call.tool_name,
                                            tool_call.payload,
                                            exec_context,
                                        )
                                        .await
                                }
                            }
                        } else {
                            self.tool_runner
                                .execute(&tool_call.tool_name, tool_call.payload, exec_context)
                                .await
                        }
                    };

                    match tool_result {
                        Ok(result) => {
                            let memory_mutating_tool = matches!(
                                tool_call.tool_name.as_str(),
                                "memory-write" | "archival-insert"
                            );
                            crate::metrics::record_tool_execution(
                                &tool_call.tool_name,
                                tool_start.elapsed().as_millis() as u64,
                                true,
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({ "tool": tool_call.tool_name }),
                                severity: agentos_audit::AuditSeverity::Info,
                                reversible: snapshot_ref.is_some(),
                                rollback_ref: snapshot_ref.clone(),
                            });

                            // Intercept kernel actions from tool results
                            let context_result = if let Some(action) =
                                crate::kernel_action::KernelAction::from_tool_result(&result)
                            {
                                let memory_mutating_action = matches!(
                                    &action,
                                    crate::kernel_action::KernelAction::MemoryBlockWrite { .. }
                                        | crate::kernel_action::KernelAction::MemoryBlockDelete { .. }
                                );
                                tracing::info!(
                                    "Task {} kernel action intercepted from tool '{}'",
                                    task.id,
                                    tool_call.tool_name,
                                );
                                let action_result =
                                    self.dispatch_kernel_action(task, action, trace_id).await;
                                if memory_mutating_action {
                                    refresh_knowledge_blocks = true;
                                }
                                action_result.result
                            } else {
                                result.clone()
                            };

                            // --- Injection scan on tool output ---
                            let result_str = context_result.to_string();
                            let scan = self.injection_scanner.scan(&result_str);
                            if scan.is_suspicious {
                                let pattern_names: Vec<&str> =
                                    scan.matches.iter().map(|m| m.pattern_name).collect();
                                let threat = format!("{:?}", scan.max_threat);
                                tracing::warn!(
                                    "Task {} tool '{}' output contains injection patterns: {:?} (threat: {})",
                                    task.id, tool_call.tool_name, pattern_names, threat
                                );
                                self.audit_log(agentos_audit::AuditEntry {
                                    timestamp: chrono::Utc::now(),
                                    trace_id: *task_trace_id,
                                    event_type: agentos_audit::AuditEventType::RiskEscalation,
                                    agent_id: Some(task.agent_id),
                                    task_id: Some(task.id),
                                    tool_id: None,
                                    details: serde_json::json!({
                                        "injection_scan": true,
                                        "tool": tool_call.tool_name,
                                        "patterns": pattern_names,
                                        "max_threat": threat,
                                    }),
                                    severity: agentos_audit::AuditSeverity::Security,
                                    reversible: false,
                                    rollback_ref: None,
                                });

                                let threat_level = scan
                                    .max_threat
                                    .as_ref()
                                    .map(|t| format!("{:?}", t))
                                    .unwrap_or_else(|| "unknown".to_string());
                                let severity = match scan.max_threat {
                                    Some(ThreatLevel::High) => EventSeverity::Critical,
                                    Some(ThreatLevel::Medium) => EventSeverity::Warning,
                                    Some(ThreatLevel::Low) | None => EventSeverity::Info,
                                };
                                let chain_depth = task
                                    .trigger_source
                                    .as_ref()
                                    .map(|ts| ts.chain_depth + 1)
                                    .unwrap_or(0);
                                self.emit_event_with_trace(
                                    EventType::PromptInjectionAttempt,
                                    EventSource::SecurityEngine,
                                    severity,
                                    serde_json::json!({
                                        "task_id": task.id.to_string(),
                                        "agent_id": task.agent_id.to_string(),
                                        "source": "tool_output",
                                        "tool_name": tool_call.tool_name,
                                        "threat_level": threat_level,
                                        "pattern_count": scan.matches.len(),
                                        "patterns": scan.matches.iter().map(|m| m.pattern_name).collect::<Vec<_>>(),
                                        "agent_intent_payload": tool_payload_preview.clone(),
                                        "suspicious_content": Self::truncate_for_prompt_payload(&result_str, 600),
                                        "preceding_tool_result": Self::truncate_for_prompt_payload(&result_str, 600),
                                    }),
                                    chain_depth,
                                    Some(*task_trace_id),
                                )
                                .await;

                                // High-confidence injection: block execution and require human
                                // review before this output enters agent context (Spec §6).
                                if scan.max_threat
                                    == Some(crate::injection_scanner::ThreatLevel::High)
                                {
                                    // Include a truncated excerpt of the suspicious content so
                                    // the human reviewer can make an informed allow/deny decision.
                                    let content_excerpt =
                                        Self::truncate_for_prompt_payload(&result_str, 300);
                                    self.escalation_manager
                                        .create_escalation(
                                            task.id,
                                            task.agent_id,
                                            crate::kernel_action::EscalationReason::SafetyConcern,
                                            format!(
                                                "Tool '{}' returned output with high-confidence injection patterns: {:?}. Suspicious content (truncated): {}",
                                                tool_call.tool_name, pattern_names, content_excerpt
                                            ),
                                            "Review the tool output before allowing it into agent context.".to_string(),
                                            vec![
                                                "Allow — inject into context".to_string(),
                                                "Deny — discard output".to_string(),
                                            ],
                                            "high".to_string(),
                                            true,
                                            trace_id,
                                            None, // auto_action: default deny on expiry
                                        )
                                        .await;
                                    self.scheduler
                                        .update_state(&task.id, TaskState::Waiting)
                                        .await
                                        .ok();
                                    // Do NOT call remove_context here: preserving conversation
                                    // history allows the task to resume with context intact if
                                    // the escalation is approved. The tainted output is never
                                    // pushed to context (bail happens before push_tool_result).
                                    self.intent_validator.remove_task(&task.id).await;
                                    anyhow::bail!(
                                        "Task paused: high-confidence injection in output of tool '{}'",
                                        tool_call.tool_name
                                    );
                                }
                            }

                            // Wrap tool output with taint tags for context safety
                            let source = format!("tool:{}", tool_call.tool_name);
                            let wrapped = crate::injection_scanner::InjectionScanner::taint_wrap(
                                &result_str,
                                &source,
                                &scan,
                            );
                            let tainted_result = serde_json::json!({ "output": wrapped });

                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &tainted_result)
                                .await
                                .ok();

                            // Structured memory extraction (non-blocking):
                            // parse typed tool output and write salient facts into semantic memory.
                            {
                                let extraction_engine = self.memory_extraction.clone();
                                let tool_name = tool_call.tool_name.clone();
                                let extraction_result = context_result.clone();
                                let extraction_ctx = crate::memory_extraction::ExtractionContext {
                                    tool_name: tool_call.tool_name.clone(),
                                    agent_id: task.agent_id,
                                    task_id: task.id,
                                };
                                let event_sender = self.event_sender.clone();
                                let capability_engine = self.capability_engine.clone();
                                let audit = self.audit.clone();
                                let extraction_chain_depth = task
                                    .trigger_source
                                    .as_ref()
                                    .map(|ts| ts.chain_depth + 1)
                                    .unwrap_or(0);
                                tokio::spawn(async move {
                                    match extraction_engine
                                        .process_tool_result(
                                            &tool_name,
                                            &extraction_result,
                                            &extraction_ctx,
                                        )
                                        .await
                                    {
                                        Ok(report) if report.updated > 0 => {
                                            crate::event_dispatch::emit_signed_event(
                                                &capability_engine,
                                                &audit,
                                                &event_sender,
                                                EventType::SemanticMemoryConflict,
                                                EventSource::MemoryArbiter,
                                                EventSeverity::Warning,
                                                serde_json::json!({
                                                    "agent_id": extraction_ctx.agent_id.to_string(),
                                                    "tool_name": tool_name,
                                                    "conflict_type": "semantic_update",
                                                    "updated_count": report.updated,
                                                }),
                                                extraction_chain_depth,
                                                TraceID::new(),
                                                Some(extraction_ctx.agent_id),
                                                Some(extraction_ctx.task_id),
                                            );
                                        }
                                        Ok(_) => {}
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "Memory extraction failed for tool '{}'",
                                                tool_name
                                            );
                                        }
                                    }
                                });
                            }
                            if memory_mutating_tool {
                                refresh_knowledge_blocks = true;
                            }

                            // Spec §11: if token budget hit 95%, take a checkpoint now
                            if self.context_manager.drain_checkpoint_flag(&task.id).await {
                                self.take_snapshot(&task.id, "escalation_required", None)
                                    .await;
                            }

                            if let Err(e) =
                                self.episodic_memory
                                    .record(agentos_memory::EpisodeRecordInput {
                                        task_id: &task.id,
                                        agent_id: &task.agent_id,
                                        entry_type: agentos_memory::EpisodeType::ToolResult,
                                        content: &context_result.to_string(),
                                        summary: Some(&format!(
                                            "Tool '{}' succeeded",
                                            tool_call.tool_name
                                        )),
                                        metadata: Some(serde_json::json!({
                                            "tool": tool_call.tool_name,
                                            "success": true,
                                            "iteration": iteration,
                                        })),
                                        trace_id: &trace_id,
                                    })
                            {
                                tracing::warn!(task_id = %task.id, error = %e, "Failed to record episodic memory");
                            }
                        }
                        Err(e) => {
                            crate::metrics::record_tool_execution(
                                &tool_call.tool_name,
                                tool_start.elapsed().as_millis() as u64,
                                false,
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::ToolExecutionFailed,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({ "tool": tool_call.tool_name, "error": e.to_string() }),
                                severity: agentos_audit::AuditSeverity::Error,
                                reversible: false,
                                rollback_ref: None,
                            });

                            let chain_depth = task
                                .trigger_source
                                .as_ref()
                                .map(|ts| ts.chain_depth + 1)
                                .unwrap_or(0);
                            self.emit_event_with_trace(
                                EventType::ToolExecutionFailed,
                                EventSource::ToolRunner,
                                EventSeverity::Warning,
                                serde_json::json!({
                                    "task_id": task.id.to_string(),
                                    "agent_id": task.agent_id.to_string(),
                                    "tool_name": tool_call.tool_name,
                                    "error": e.to_string(),
                                }),
                                chain_depth,
                                Some(trace_id),
                            )
                            .await;

                            let error_result = serde_json::json!({
                                "error": e.to_string()
                            });
                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                .await
                                .ok();

                            if let Err(record_err) =
                                self.episodic_memory
                                    .record(agentos_memory::EpisodeRecordInput {
                                        task_id: &task.id,
                                        agent_id: &task.agent_id,
                                        entry_type: agentos_memory::EpisodeType::ToolResult,
                                        content: &error_result.to_string(),
                                        summary: Some(&format!(
                                            "Tool '{}' failed: {}",
                                            tool_call.tool_name, e
                                        )),
                                        metadata: Some(serde_json::json!({
                                            "tool": tool_call.tool_name,
                                            "success": false,
                                            "iteration": iteration,
                                            "error": e.to_string(),
                                        })),
                                        trace_id: &trace_id,
                                    })
                            {
                                tracing::warn!(task_id = %task.id, error = %record_err, "Failed to record episodic memory");
                            }
                        }
                    }
                }
                None => {
                    // No tool call — this is the final answer
                    final_answer = inference.text;
                    break;
                }
            }
        }

        if final_answer.is_empty() {
            if completed_iterations >= max_iterations as u32 {
                anyhow::bail!("Max iterations exceeded without producing final answer");
            }
            anyhow::bail!("Task ended without producing final answer");
        }

        // Task success episodic write moved to execute_task() where duration_ms is available.

        self.context_manager.remove_context(&task.id).await;
        self.intent_validator.remove_task(&task.id).await;
        Ok(TaskResult {
            answer: final_answer,
            tool_call_count,
            iterations: completed_iterations,
        })
    }

    /// Execute a task from the background executor loop.
    pub(crate) async fn execute_task(&self, task: &AgentTask) {
        let start = std::time::Instant::now();
        let task_trace_id = TraceID::new();
        crate::metrics::record_task_queued();

        // Transition to Running — bail out if the task is already terminal
        // (e.g. cancelled before execution started).
        let transitioned = self
            .scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Running)
            .await
            .unwrap_or(false);
        if !transitioned {
            tracing::info!(
                task_id = %task.id,
                "Task already in terminal state before execution, skipping"
            );
            return;
        }
        self.scheduler.mark_started(&task.id).await.ok();

        self.emit_event_with_trace(
            EventType::TaskStarted,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "task_id": task.id.to_string(),
                "agent_id": task.agent_id.to_string(),
                "prompt_preview": task.original_prompt.chars().take(200).collect::<String>(),
            }),
            0,
            Some(task_trace_id),
        )
        .await;

        match self.execute_task_sync(task, &task_trace_id).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                self.complete_task_success(task, &result, duration_ms, task_trace_id)
                    .await;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                self.complete_task_failure(task, e, duration_ms, task_trace_id)
                    .await;
            }
        }
    }

    /// Build the agent directory block for inclusion in compiled context.
    /// Lists all registered agents except `exclude_agent_id` with their
    /// status, model, provider, and permissions.
    pub(crate) async fn build_agent_directory(&self, exclude_agent_id: &AgentID) -> String {
        let mut directory = String::from(
            "\n\n[AGENT_DIRECTORY]\nYou are operating inside AgentOS. \
             The following agents are available:\n",
        );

        let agents = self
            .agent_registry
            .read()
            .await
            .list_all()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();

        for agent in agents {
            if agent.id == *exclude_agent_id {
                continue;
            }
            let status = match agent.current_task {
                Some(tid) => format!("Busy ({})", tid),
                None => "Idle".to_string(),
            };
            let perms = self
                .capability_engine
                .get_permissions(&agent.id)
                .unwrap_or_default();
            let mut perm_strs = Vec::new();
            for e in perms.entries {
                let r = if e.read { "r" } else { "" };
                let w = if e.write { "w" } else { "" };
                let x = if e.execute { "x" } else { "" };
                perm_strs.push(format!("{}:{}{}{}", e.resource, r, w, x));
            }
            let perm_str = if perm_strs.is_empty() {
                "None".to_string()
            } else {
                perm_strs.join(", ")
            };
            let provider_str = match agent.provider {
                agentos_types::LLMProvider::Anthropic => "anthropic",
                agentos_types::LLMProvider::OpenAI => "openai",
                agentos_types::LLMProvider::Ollama => "ollama",
                agentos_types::LLMProvider::Gemini => "gemini",
                agentos_types::LLMProvider::Custom(_) => "custom",
            };
            directory.push_str(&format!(
                "\n- {} ({}/{}) — Status: {}\n  Permissions: {}",
                agent.name, provider_str, agent.model, status, perm_str
            ));
        }

        directory.push_str(
            "\n\nTo message an agent: use the agent-message tool\n\
             To delegate a subtask: use the task-delegate tool\n\
             [/AGENT_DIRECTORY]",
        );

        directory
    }

    pub(crate) fn truncate_for_prompt_payload(input: &str, max_chars: usize) -> String {
        input.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_failure_marks_paused_tasks() {
        let (reason, severity, is_pause) =
            Kernel::classify_task_failure("Task paused: high-confidence injection detected");
        assert_eq!(reason, "task_paused");
        assert_eq!(severity, EventSeverity::Warning);
        assert!(is_pause);
    }

    #[test]
    fn classify_failure_marks_max_iteration_failures() {
        let (reason, severity, is_pause) =
            Kernel::classify_task_failure("Max iterations exceeded without producing final answer");
        assert_eq!(reason, "max_iterations");
        assert_eq!(severity, EventSeverity::Warning);
        assert!(!is_pause);
    }
}
