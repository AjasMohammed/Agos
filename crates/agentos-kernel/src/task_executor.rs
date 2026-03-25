use crate::event_bus::{parse_event_type_filter, parse_subscription_priority};
use crate::injection_scanner::ThreatLevel;
use crate::kernel::Kernel;
use agentos_sandbox::{SandboxConfig, SandboxExecRequest, SandboxExecutor};
use agentos_tools::traits::ToolExecutionContext;
use agentos_tools::{tool_category_with_weight, ToolCategory};
use agentos_types::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tracing::Instrument;

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
        if lower.starts_with("task suspended:") {
            return ("task_suspended", EventSeverity::Warning, true);
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

    fn resolve_task_max_iterations(
        task: &AgentTask,
        task_limits: &crate::config::TaskLimitsConfig,
        autonomous_config: &crate::config::AutonomousModeConfig,
    ) -> u32 {
        // Autonomous tasks use the autonomous_mode ceiling — effectively unlimited
        // for any real-world workflow, but still bounded to prevent infinite loops
        // caused by bugs rather than intentional long-running work.
        if task.autonomous {
            return autonomous_config.max_iterations.max(1);
        }
        let resolved = if let Some(max_iterations) = task.max_iterations {
            max_iterations
        } else {
            match task
                .reasoning_hints
                .as_ref()
                .map(|hints| hints.estimated_complexity)
                .unwrap_or(ComplexityLevel::Low)
            {
                ComplexityLevel::Low => task_limits.max_iterations_low,
                ComplexityLevel::Medium => task_limits.max_iterations_medium,
                ComplexityLevel::High => task_limits.max_iterations_high,
            }
        };
        // Ensure at least 1 iteration to avoid silent no-ops.
        resolved.max(1)
    }

    fn sandbox_overhead_for_category(category: ToolCategory) -> u64 {
        match category {
            ToolCategory::Stateless => SandboxConfig::OVERHEAD_STATELESS,
            ToolCategory::Memory => SandboxConfig::OVERHEAD_MEMORY,
            ToolCategory::Network => SandboxConfig::OVERHEAD_NETWORK,
            ToolCategory::Hal => SandboxConfig::OVERHEAD_HAL,
        }
    }

    async fn sandbox_plan_for_tool(
        &self,
        tool_name: &str,
    ) -> Option<(SandboxConfig, u64, Option<String>)> {
        let registry = self.tool_registry.read().await;
        let tool = registry.get_by_name(tool_name)?;

        if tool.manifest.executor.executor_type != ExecutorType::Inline {
            return None;
        }

        let manifest_weight = tool.manifest.sandbox.weight.clone();
        // Kernel-context and special tools (agent-list, task-list, agent-self, etc.)
        // return None from tool_category_with_weight — they must execute in-process,
        // not in a sandbox child where they lack access to kernel state.
        let category = tool_category_with_weight(tool_name, manifest_weight.as_deref())?;

        // Check sandbox policy against tool trust tier.
        let trust_tier = tool.manifest.manifest.trust_tier;
        let should_sandbox = should_sandbox_tool(self.config.kernel.sandbox_policy, trust_tier);

        tracing::debug!(
            tool = tool_name,
            ?trust_tier,
            sandbox_policy = ?self.config.kernel.sandbox_policy,
            should_sandbox,
            "Sandbox dispatch decision"
        );

        if !should_sandbox {
            return None;
        }

        let config = SandboxConfig::from_manifest(&tool.manifest.sandbox);
        let overhead_bytes = Self::sandbox_overhead_for_category(category);
        Some((config, overhead_bytes, manifest_weight))
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

    async fn execute_parallel_tool_calls(
        &self,
        task: &AgentTask,
        task_trace_id: &TraceID,
        iteration: u32,
        mut tool_calls: Vec<crate::tool_call::ParsedToolCall>,
        tool_call_count: &mut u32,
        refresh_knowledge_blocks: &mut bool,
    ) -> Result<(), anyhow::Error> {
        let mut consecutive_push_failures: u32 = 0;
        struct PreparedParallelToolCall {
            order: usize,
            tool_call: crate::tool_call::ParsedToolCall,
            trace_id: TraceID,
            snapshot_ref: Option<String>,
            tool_payload_preview: String,
            sandbox_plan: Option<(SandboxConfig, u64, Option<String>)>,
        }

        struct ParallelToolOutcome {
            order: usize,
            tool_call: crate::tool_call::ParsedToolCall,
            trace_id: TraceID,
            snapshot_ref: Option<String>,
            tool_payload_preview: String,
            duration_ms: u64,
            result: Result<serde_json::Value, AgentOSError>,
            /// "sandbox" or "in_process" — for audit and tracing.
            execution_mode: &'static str,
        }

        let max_parallel = if task.autonomous {
            self.config
                .kernel
                .autonomous_mode
                .max_parallel_tool_calls
                .max(1)
        } else {
            self.config.kernel.tool_calls.max_parallel.max(1)
        };
        if tool_calls.len() > max_parallel {
            tracing::warn!(
                task_id = %task.id,
                requested = tool_calls.len(),
                max_parallel,
                "Truncating parsed tool calls to max_parallel limit"
            );
            let skipped_calls: Vec<_> = tool_calls.drain(max_parallel..).collect();
            for skipped in skipped_calls {
                let error_result = serde_json::json!({
                    "error": format!(
                        "Skipped tool call because max_parallel limit ({}) was reached",
                        max_parallel
                    )
                });
                if let Err(e) = self
                    .context_manager
                    .push_tool_result(
                        &task.id,
                        &skipped.tool_name,
                        &error_result,
                        skipped.id.clone(),
                    )
                    .await
                {
                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    consecutive_push_failures += 1;
                    if consecutive_push_failures >= 3 {
                        anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                    }
                } else {
                    consecutive_push_failures = 0;
                }
            }
        }

        // Tracks a HardLimitExceeded hit during the preparation loop so we can
        // handle it (Suspend or Kill) after the loop exits.
        let mut batch_budget_exceeded: Option<(BudgetAction, String)> = None;

        let mut prepared = Vec::new();
        for (order, tool_call) in tool_calls.into_iter().enumerate() {
            let trace_id = TraceID::new();

            tracing::info!(
                task_id = %task.id,
                tool = %tool_call.tool_name,
                intent = ?tool_call.intent_type,
                "Task parsed tool call (parallel batch)"
            );

            // Explicitly gate by registered tool identity first.
            let chain_depth = task
                .trigger_source
                .as_ref()
                .map(|ts| ts.chain_depth + 1)
                .unwrap_or(0);

            let requested_tool_id = {
                let registry = self.tool_registry.read().await;
                match registry.get_by_name(&tool_call.tool_name) {
                    Some(tool) => tool.id,
                    None => {
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
                                "context": "parallel_batch",
                            }),
                            severity: agentos_audit::AuditSeverity::Security,
                            reversible: false,
                            rollback_ref: None,
                        });
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
                                "context": "parallel_batch",
                            }),
                            chain_depth,
                            Some(trace_id),
                            Some(task.agent_id),
                            Some(task.id),
                        )
                        .await;
                        let error_result = serde_json::json!({
                            "error": format!("Unknown tool requested: {}", tool_call.tool_name)
                        });
                        if let Err(e) = self
                            .context_manager
                            .push_tool_result(
                                &task.id,
                                &tool_call.tool_name,
                                &error_result,
                                tool_call.id.clone(),
                            )
                            .await
                        {
                            tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            consecutive_push_failures += 1;
                            if consecutive_push_failures >= 3 {
                                anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                            }
                        } else {
                            consecutive_push_failures = 0;
                        }
                        continue;
                    }
                }
            };

            if !task.capability_token.allowed_tools.is_empty()
                && !task
                    .capability_token
                    .allowed_tools
                    .contains(&requested_tool_id)
            {
                let allowed_tool_names = {
                    let registry = self.tool_registry.read().await;
                    task.capability_token
                        .allowed_tools
                        .iter()
                        .map(|tool_id| {
                            registry
                                .get_by_id(tool_id)
                                .map(|tool| tool.manifest.manifest.name.clone())
                                .unwrap_or_else(|| tool_id.to_string())
                        })
                        .collect::<Vec<_>>()
                };
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
                        "context": "parallel_batch",
                    }),
                    severity: agentos_audit::AuditSeverity::Security,
                    reversible: false,
                    rollback_ref: None,
                });
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
                        "context": "parallel_batch",
                    }),
                    chain_depth,
                    Some(trace_id),
                    Some(task.agent_id),
                    Some(task.id),
                )
                .await;
                let error_result = serde_json::json!({
                    "error": format!("Unauthorized tool access blocked: {}", tool_call.tool_name)
                });
                if let Err(e) = self
                    .context_manager
                    .push_tool_result(
                        &task.id,
                        &tool_call.tool_name,
                        &error_result,
                        tool_call.id.clone(),
                    )
                    .await
                {
                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    consecutive_push_failures += 1;
                    if consecutive_push_failures >= 3 {
                        anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                    }
                } else {
                    consecutive_push_failures = 0;
                }
                continue;
            }

            match self
                .validate_tool_call_full(task, &tool_call, trace_id)
                .await
            {
                Err(denial_reason) => {
                    tracing::warn!(
                        task_id = %task.id,
                        tool = %tool_call.tool_name,
                        reason = %denial_reason,
                        "Parallel tool-call validation denied"
                    );
                    let error_result = serde_json::json!({
                        "error": format!("Permission denied: {}", denial_reason)
                    });
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &error_result,
                            tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                        consecutive_push_failures += 1;
                        if consecutive_push_failures >= 3 {
                            anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                        }
                    } else {
                        consecutive_push_failures = 0;
                    }
                    continue;
                }
                Ok(IntentCoherenceResult::Rejected { reason }) => {
                    tracing::warn!(
                        task_id = %task.id,
                        tool = %tool_call.tool_name,
                        reason = %reason,
                        "Parallel tool-call coherence rejected"
                    );
                    let error_result = serde_json::json!({
                        "error": format!("Coherence check failed: {}", reason)
                    });
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &error_result,
                            tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                        consecutive_push_failures += 1;
                        if consecutive_push_failures >= 3 {
                            anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                        }
                    } else {
                        consecutive_push_failures = 0;
                    }
                    continue;
                }
                Ok(IntentCoherenceResult::Suspicious { reason, .. }) => {
                    // Inject loop warning into context so the LLM knows it is repeating itself
                    let warning = serde_json::json!({
                        "warning": format!("LOOP DETECTED: {}. You are repeating the same action. Try a different approach or complete the task with the information you already have.", reason)
                    });
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &warning,
                            tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                        consecutive_push_failures += 1;
                        if consecutive_push_failures >= 3 {
                            anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                        }
                    } else {
                        consecutive_push_failures = 0;
                    }
                }
                Ok(IntentCoherenceResult::Approved) => {}
            }

            if matches!(
                tool_call.intent_type,
                IntentType::Subscribe | IntentType::Unsubscribe
            ) {
                self.intent_validator
                    .record_tool_call(&task.id, &tool_call)
                    .await;
                *tool_call_count += 1;
                let dynamic_result = self
                    .handle_dynamic_event_subscription_intent(task, &tool_call, trace_id)
                    .await;
                let context_result = match dynamic_result {
                    Ok(value) => value,
                    Err(err) => serde_json::json!({ "error": err }),
                };
                if let Err(e) = self
                    .context_manager
                    .push_tool_result(
                        &task.id,
                        &tool_call.tool_name,
                        &context_result,
                        tool_call.id.clone(),
                    )
                    .await
                {
                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    consecutive_push_failures += 1;
                    if consecutive_push_failures >= 3 {
                        anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                    }
                } else {
                    consecutive_push_failures = 0;
                }
                continue;
            }

            // Check budget BEFORE incrementing counters so we don't count calls
            // that never execute.
            let tool_budget = self.cost_tracker.record_tool_call(&task.agent_id).await;
            if let crate::cost_tracker::BudgetCheckResult::HardLimitExceeded { resource, action } =
                &tool_budget
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
                        "context": "parallel_batch",
                    }),
                    severity: agentos_audit::AuditSeverity::Security,
                    reversible: false,
                    rollback_ref: None,
                });
                let error_result = serde_json::json!({
                    "error": "Tool call budget exceeded"
                });
                if let Err(e) = self
                    .context_manager
                    .push_tool_result(
                        &task.id,
                        &tool_call.tool_name,
                        &error_result,
                        tool_call.id.clone(),
                    )
                    .await
                {
                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    consecutive_push_failures += 1;
                    if consecutive_push_failures >= 3 {
                        anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                    }
                }
                // Note: no else-reset here because `break` follows immediately —
                // the counter won't be read again in this loop iteration.
                batch_budget_exceeded = Some((*action, resource.clone()));
                break;
            }

            self.intent_validator
                .record_tool_call(&task.id, &tool_call)
                .await;
            *tool_call_count += 1;

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
                    let error_result = serde_json::json!({
                        "error": "Action forbidden by security policy"
                    });
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &error_result,
                            tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    }
                    continue;
                }
                ActionRiskLevel::HardApproval => {
                    let waiting_result = serde_json::json!({
                        "status": "awaiting_approval",
                        "message": "This action requires human approval and was skipped from the parallel batch."
                    });
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &waiting_result,
                            tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    }
                    continue;
                }
                ActionRiskLevel::SoftApproval
                | ActionRiskLevel::Notify
                | ActionRiskLevel::Autonomous => {}
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

            let snapshot_ref = if tool_call.intent_type == IntentType::Write
                || tool_call.intent_type == IntentType::Execute
            {
                self.take_snapshot(&task.id, &tool_call.tool_name, Some(&tool_call.payload))
                    .await
            } else {
                None
            };
            let tool_payload_preview = Self::truncate_for_prompt_payload(
                &serde_json::to_string(&tool_call.payload).unwrap_or_default(),
                600,
            );
            let sandbox_plan = self.sandbox_plan_for_tool(&tool_call.tool_name).await;

            prepared.push(PreparedParallelToolCall {
                order,
                tool_call,
                trace_id,
                snapshot_ref,
                tool_payload_preview,
                sandbox_plan,
            });
        }

        // Enforce budget action after the preparation loop.
        if let Some((action, resource)) = batch_budget_exceeded {
            self.context_manager.remove_context(&task.id).await;
            self.intent_validator.remove_task(&task.id).await;
            if action == BudgetAction::Suspend {
                match self
                    .scheduler
                    .update_state_if_not_terminal(&task.id, TaskState::Suspended)
                    .await
                {
                    Ok(true) => {
                        self.emit_event_with_trace(
                            EventType::TaskSuspended,
                            EventSource::TaskScheduler,
                            EventSeverity::Warning,
                            serde_json::json!({
                                "task_id": task.id.to_string(),
                                "agent_id": task.agent_id.to_string(),
                                "resource": resource,
                                "reason": "budget_tool_call_limit_suspend_parallel",
                            }),
                            0,
                            Some(*task_trace_id),
                            Some(task.agent_id),
                            Some(task.id),
                        )
                        .await;
                        anyhow::bail!(
                            "task suspended: tool call budget hard limit reached: {}",
                            resource
                        );
                    }
                    Ok(false) => {
                        tracing::warn!(
                            task_id = %task.id,
                            "Budget suspension (parallel batch): task already terminal"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            task_id = %task.id,
                            error = %e,
                            "Failed to set task to Suspended during parallel batch budget enforcement"
                        );
                    }
                }
            }
            return Err(anyhow::Error::new(AgentOSError::BudgetExceeded {
                agent_id: task.agent_id.to_string(),
                detail: format!("tool call hard limit exceeded: {}", resource),
            }));
        }

        if prepared.is_empty() {
            return Ok(());
        }

        let agent_snapshot = {
            let registry = self.agent_registry.read().await;
            let agents: Vec<AgentSummary> = registry
                .list_all()
                .into_iter()
                .map(|p| AgentSummary {
                    id: p.id,
                    name: p.name.clone(),
                    status: format!("{:?}", p.status).to_lowercase(),
                    registered_at: p.created_at,
                })
                .collect();
            AgentRegistrySnapshot::new(agents)
        };
        let task_snapshot = self.scheduler.snapshot_tasks().await;
        let escalation_snapshot = {
            let pending = self.escalation_manager.list_pending().await;
            let agent_id = task.agent_id;
            let summaries: Vec<EscalationSummary> = pending
                .into_iter()
                .filter(|e| e.agent_id == agent_id)
                .map(|e| EscalationSummary {
                    id: e.id,
                    task_id: e.task_id,
                    agent_id: e.agent_id,
                    reason: format!("{:?}", e.reason),
                    context_summary: e.context_summary,
                    decision_point: e.decision_point,
                    options: e.options,
                    urgency: e.urgency,
                    blocking: e.blocking,
                    created_at: e.created_at,
                    expires_at: e.expires_at,
                    resolved: e.resolved,
                    resolution: e.resolution,
                })
                .collect();
            EscalationSnapshot::new(summaries)
        };
        let agent_snapshot_ref: Arc<dyn AgentRegistryQuery> = Arc::new(agent_snapshot);
        let task_snapshot_ref: Arc<dyn TaskQuery> = Arc::new(task_snapshot);
        let escalation_snapshot_ref: Arc<dyn EscalationQuery> = Arc::new(escalation_snapshot);

        let fallback_timeout_secs = if task.autonomous {
            self.config.kernel.autonomous_mode.tool_timeout_seconds
        } else {
            self.config.kernel.tool_execution.default_timeout_seconds
        };
        let mut join_set = JoinSet::new();
        for call in prepared {
            let tool_runner = self.tool_runner.clone();
            let sandbox = self.sandbox.clone();
            let data_dir = self.data_dir.clone();
            let workspace_paths = self.workspace_paths.clone();
            let task_id = task.id;
            let agent_id = task.agent_id;
            let trace_id = call.trace_id;
            let permissions = task.capability_token.permissions.clone();
            let vault = self.vault.clone();
            let hal = self.hal.clone();
            let agent_registry = agent_snapshot_ref.clone();
            let task_registry = task_snapshot_ref.clone();
            let escalation_query = escalation_snapshot_ref.clone();
            let order = call.order;
            let snapshot_ref = call.snapshot_ref;
            let tool_payload_preview = call.tool_payload_preview;
            let tool_call = call.tool_call;
            let sandbox_plan = call.sandbox_plan;
            let tool_cancellation = self.cancellation_token.child_token();
            let execution_mode: &'static str = if sandbox_plan.is_some() {
                "sandbox"
            } else {
                "in_process"
            };

            self.emit_event_with_trace(
                EventType::ToolCallStarted,
                EventSource::ToolRunner,
                EventSeverity::Info,
                serde_json::json!({
                    "tool_name": tool_call.tool_name,
                    "task_id": task.id.to_string(),
                    "agent_id": task.agent_id.to_string(),
                    "execution_mode": execution_mode,
                }),
                task.trigger_source
                    .as_ref()
                    .map(|ts| ts.chain_depth + 1)
                    .unwrap_or(0),
                Some(trace_id),
                Some(task.agent_id),
                Some(task.id),
            )
            .await;

            let tool_span = tracing::info_span!(
                "tool_execution",
                tool = %tool_call.tool_name,
                mode = execution_mode,
                task_id = %task_id,
            );
            join_set.spawn(
                async move {
                    let sandbox_permissions = permissions.clone();
                    let sandbox_workspace_paths = workspace_paths.clone();
                    let exec_context = ToolExecutionContext {
                        data_dir,
                        task_id,
                        agent_id,
                        trace_id,
                        permissions,
                        vault: Some(Arc::new(agentos_vault::ProxyVault::new(vault))),
                        hal: Some(hal),
                        // ToolRunner::execute() always overrides this with the shared registry.
                        file_lock_registry: None,
                        agent_registry: Some(agent_registry),
                        task_registry: Some(task_registry),
                        escalation_query: Some(escalation_query),
                        workspace_paths,
                        cancellation_token: tool_cancellation,
                    };

                    let tool_start = std::time::Instant::now();
                    let payload = tool_call.payload.clone();
                    let result = if let Some((config, category_overhead_bytes, manifest_weight)) =
                        sandbox_plan
                    {
                        let timeout = Duration::from_millis(config.max_cpu_ms.max(5000));
                        let request = SandboxExecRequest {
                            tool_name: tool_call.tool_name.clone(),
                            payload,
                            data_dir: exec_context.data_dir.clone(),
                            manifest_weight,
                            task_id: Some(exec_context.task_id),
                            agent_id: Some(exec_context.agent_id),
                            trace_id: Some(exec_context.trace_id),
                            permissions: sandbox_permissions,
                            workspace_paths: Some(sandbox_workspace_paths),
                        };
                        match sandbox
                            .spawn(request, &config, timeout, category_overhead_bytes)
                            .await
                        {
                            Ok(sandbox_result) => SandboxExecutor::parse_result(&sandbox_result),
                            Err(e) => {
                                tracing::error!(
                                    tool = %tool_call.tool_name,
                                    error = %e,
                                    "Sandbox spawn failed — refusing unsandboxed execution"
                                );
                                Err(e)
                            }
                        }
                    } else {
                        let execute_fut =
                            tool_runner.execute(&tool_call.tool_name, payload, exec_context);
                        match tokio::time::timeout(
                            Duration::from_secs(fallback_timeout_secs),
                            execute_fut,
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => {
                                tracing::warn!(
                                    tool = %tool_call.tool_name,
                                    timeout_secs = fallback_timeout_secs,
                                    "In-process tool call timed out"
                                );
                                Err(agentos_types::AgentOSError::ToolExecutionFailed {
                                    tool_name: tool_call.tool_name.clone(),
                                    reason: format!("timed out after {}s", fallback_timeout_secs),
                                })
                            }
                        }
                    };

                    ParallelToolOutcome {
                        order,
                        tool_call,
                        trace_id,
                        snapshot_ref,
                        tool_payload_preview,
                        duration_ms: tool_start.elapsed().as_millis() as u64,
                        result,
                        execution_mode,
                    }
                }
                .instrument(tool_span),
            );
        }

        let mut outcomes: Vec<ParallelToolOutcome> = Vec::new();
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => {
                    tracing::error!(
                        task_id = %task.id,
                        error = %error,
                        "Parallel tool call task join failed"
                    );
                }
            }
        }
        outcomes.sort_by_key(|o| o.order);

        for outcome in outcomes {
            match outcome.result {
                Ok(result) => {
                    let memory_mutating_tool = matches!(
                        outcome.tool_call.tool_name.as_str(),
                        "memory-write" | "archival-insert"
                    );
                    if memory_mutating_tool {
                        *refresh_knowledge_blocks = true;
                    }
                    crate::metrics::record_tool_execution(
                        &outcome.tool_call.tool_name,
                        outcome.duration_ms,
                        true,
                    );
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: outcome.trace_id,
                        event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({ "tool": outcome.tool_call.tool_name }),
                        severity: agentos_audit::AuditSeverity::Info,
                        reversible: outcome.snapshot_ref.is_some(),
                        rollback_ref: outcome.snapshot_ref.clone(),
                    });
                    {
                        let chain_depth = task
                            .trigger_source
                            .as_ref()
                            .map(|ts| ts.chain_depth + 1)
                            .unwrap_or(0);
                        self.emit_event_with_trace(
                            EventType::ToolCallCompleted,
                            EventSource::ToolRunner,
                            EventSeverity::Info,
                            serde_json::json!({
                                "tool_name": outcome.tool_call.tool_name,
                                "task_id": task.id.to_string(),
                                "agent_id": task.agent_id.to_string(),
                                "duration_ms": outcome.duration_ms,
                                "execution_mode": outcome.execution_mode,
                            }),
                            chain_depth,
                            Some(outcome.trace_id),
                            Some(task.agent_id),
                            Some(task.id),
                        )
                        .await;
                    }

                    let context_result = if let Some(action) =
                        crate::kernel_action::KernelAction::from_tool_result(&result)
                    {
                        let memory_mutating_action = matches!(
                            &action,
                            crate::kernel_action::KernelAction::MemoryBlockWrite { .. }
                                | crate::kernel_action::KernelAction::MemoryBlockDelete { .. }
                        );
                        let action_result = self
                            .dispatch_kernel_action(task, action, outcome.trace_id)
                            .await;
                        if memory_mutating_action {
                            *refresh_knowledge_blocks = true;
                        }
                        action_result.result
                    } else {
                        result
                    };

                    let result_str = Self::maybe_truncate_output(
                        context_result.to_string(),
                        self.config.kernel.tool_execution.max_output_bytes,
                        &outcome.tool_call.tool_name,
                    );
                    let scan = self.injection_scanner.scan(&result_str);
                    if scan.is_suspicious {
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
                                "tool_name": outcome.tool_call.tool_name,
                                "threat_level": threat_level,
                                "pattern_count": scan.matches.len(),
                                "patterns": scan.matches.iter().map(|m| m.pattern_name).collect::<Vec<_>>(),
                                "agent_intent_payload": outcome.tool_payload_preview,
                                "suspicious_content": Self::truncate_for_prompt_payload(&result_str, 600),
                                "preceding_tool_result": Self::truncate_for_prompt_payload(&result_str, 600),
                            }),
                            chain_depth,
                            Some(*task_trace_id),
                                                Some(task.agent_id),
                        Some(task.id),
                        )
                        .await;
                    }
                    if scan.max_threat == Some(ThreatLevel::High) {
                        let blocked = serde_json::json!({
                            "error": "Tool output blocked due to high-confidence injection patterns"
                        });
                        if let Err(e) = self
                            .context_manager
                            .push_tool_result(
                                &task.id,
                                &outcome.tool_call.tool_name,
                                &blocked,
                                outcome.tool_call.id.clone(),
                            )
                            .await
                        {
                            tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                        }
                        continue;
                    }

                    let source = format!("tool:{}", outcome.tool_call.tool_name);
                    let wrapped = crate::injection_scanner::InjectionScanner::taint_wrap(
                        &result_str,
                        &source,
                        &scan,
                    );
                    let tainted_result = serde_json::json!({ "output": wrapped });
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &outcome.tool_call.tool_name,
                            &tainted_result,
                            outcome.tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    }

                    if let Err(e) = self
                        .episodic_memory
                        .record(agentos_memory::EpisodeRecordInput {
                            task_id: &task.id,
                            agent_id: &task.agent_id,
                            entry_type: agentos_memory::EpisodeType::ToolResult,
                            content: &context_result.to_string(),
                            summary: Some(&format!(
                                "Tool '{}' succeeded (parallel batch)",
                                outcome.tool_call.tool_name
                            )),
                            metadata: Some(serde_json::json!({
                                "tool": outcome.tool_call.tool_name,
                                "success": true,
                                "iteration": iteration,
                                "parallel_batch": true,
                            })),
                            trace_id: &outcome.trace_id,
                        })
                        .await
                    {
                        tracing::warn!(
                            task_id = %task.id,
                            error = %e,
                            "Failed to record episodic memory for parallel tool result"
                        );
                    }
                }
                Err(e) => {
                    crate::metrics::record_tool_execution(
                        &outcome.tool_call.tool_name,
                        outcome.duration_ms,
                        false,
                    );
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: outcome.trace_id,
                        event_type: agentos_audit::AuditEventType::ToolExecutionFailed,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "tool": outcome.tool_call.tool_name,
                            "error": e.to_string(),
                        }),
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
                            "tool_name": outcome.tool_call.tool_name,
                            "error": e.to_string(),
                            "execution_mode": outcome.execution_mode,
                        }),
                        chain_depth,
                        Some(outcome.trace_id),
                        Some(task.agent_id),
                        Some(task.id),
                    )
                    .await;

                    let error_result = serde_json::json!({
                        "error": e.to_string()
                    });
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &outcome.tool_call.tool_name,
                            &error_result,
                            outcome.tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    }

                    if let Err(record_err) = self
                        .episodic_memory
                        .record(agentos_memory::EpisodeRecordInput {
                            task_id: &task.id,
                            agent_id: &task.agent_id,
                            entry_type: agentos_memory::EpisodeType::ToolResult,
                            content: &error_result.to_string(),
                            summary: Some(&format!(
                                "Tool '{}' failed (parallel batch): {}",
                                outcome.tool_call.tool_name, e
                            )),
                            metadata: Some(serde_json::json!({
                                "tool": outcome.tool_call.tool_name,
                                "success": false,
                                "iteration": iteration,
                                "parallel_batch": true,
                                "error": e.to_string(),
                            })),
                            trace_id: &outcome.trace_id,
                        })
                        .await
                    {
                        tracing::warn!(
                            task_id = %task.id,
                            error = %record_err,
                            "Failed to record episodic memory for failed parallel tool result"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Execute a single task synchronously: assemble context, call LLM, process tool calls, repeat.
    #[tracing::instrument(skip_all, fields(task_id = %task.id, agent_id = %task.agent_id))]
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

        // Build the structured tool manifest list once per task so adapters that
        // support native function calling (e.g. OpenAI) can receive schema metadata.
        let llm_tool_manifests: Vec<ToolManifest> = {
            let registry = self.tool_registry.read().await;
            let mut manifests = if task.capability_token.allowed_tools.is_empty() {
                registry
                    .list_all()
                    .into_iter()
                    .map(|tool| tool.manifest.clone())
                    .collect::<Vec<_>>()
            } else {
                task.capability_token
                    .allowed_tools
                    .iter()
                    .filter_map(|tool_id| registry.get_by_id(tool_id).map(|t| t.manifest.clone()))
                    .collect::<Vec<_>>()
            };
            manifests.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
            manifests
        };

        // 3. Agent loop: LLM → parse → tool call → push result → repeat
        let max_iterations = Self::resolve_task_max_iterations(
            task,
            &self.config.kernel.task_limits,
            &self.config.kernel.autonomous_mode,
        );
        let mut final_answer = String::new();
        let mut tool_call_count: u32 = 0;
        let mut completed_iterations: u32 = 0;
        let mut consecutive_push_failures: u32 = 0;
        let mut knowledge_blocks: Vec<String> = Vec::new();
        let mut refresh_knowledge_blocks = true;
        let mut context_warning_emitted = false;

        for iteration in 0..max_iterations {
            completed_iterations = iteration + 1;
            let iteration_trace_id = TraceID::new();
            let raw_context = match self.context_manager.get_context(&task.id).await {
                Ok(ctx) => ctx,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        task_id = %task.id,
                        iteration = iteration,
                        "Context manager failed to fetch context — aborting task"
                    );
                    anyhow::bail!(
                        "Task aborted at iteration {}: context manager error: {}",
                        iteration,
                        e
                    );
                }
            };

            if refresh_knowledge_blocks {
                let refresh_start = std::time::Instant::now();
                knowledge_blocks.clear();

                let chain_depth = task
                    .trigger_source
                    .as_ref()
                    .map(|ts| ts.chain_depth + 1)
                    .unwrap_or(0);

                // All event-triggered tasks skip adaptive retrieval: for newly registered
                // agents they have no memories yet; for established agents the trade-off
                // (avoiding retrieval latency vs. missing memory context on event responses)
                // is acceptable because Phase 1 already ensures no MemorySearchFailed cascade
                // even when retrieval runs against empty stores.
                let is_event_triggered = task.trigger_source.is_some();

                if !retrieval_plan.is_empty() && !is_event_triggered {
                    let outcome = self
                        .retrieval_executor
                        .execute(&retrieval_plan, Some(&task.agent_id))
                        .await;

                    // Only emit MemorySearchFailed for actual infrastructure errors,
                    // not for an empty store (which is normal for a new agent).
                    if outcome.has_errors() {
                        for err in outcome.errors() {
                            tracing::warn!(
                                task_id = %task.id,
                                error = %err,
                                "Retrieval backend error (results may be partial)"
                            );
                        }
                        self.emit_event_with_trace(
                            EventType::MemorySearchFailed,
                            EventSource::MemoryArbiter,
                            EventSeverity::Warning,
                            serde_json::json!({
                                "agent_id": task.agent_id.to_string(),
                                "task_id": task.id.to_string(),
                                "search_type": "adaptive_retrieval",
                                "query_count": retrieval_plan.queries.len(),
                                "errors": outcome.errors(),
                                "partial_results": outcome.result_count() > 0,
                            }),
                            chain_depth,
                            Some(iteration_trace_id),
                            Some(task.agent_id),
                            Some(task.id),
                        )
                        .await;
                    }

                    let retrieved = outcome.into_results();
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
                } else if is_event_triggered && !retrieval_plan.is_empty() {
                    tracing::debug!(
                        task_id = %task.id,
                        chain_depth,
                        "Skipping adaptive retrieval for event-triggered task"
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
                            Some(task.agent_id),
                            Some(task.id),
                        )
                        .await;
                        context_warning_emitted = true;

                        // Emit ContextWindowExhausted at 100%
                        if utilization >= 1.0 {
                            self.emit_event_with_trace(
                                EventType::ContextWindowExhausted,
                                EventSource::ContextManager,
                                EventSeverity::Critical,
                                serde_json::json!({
                                    "task_id": task.id.to_string(),
                                    "agent_id": task.agent_id.to_string(),
                                    "action": "context_window_full",
                                }),
                                chain_depth,
                                Some(iteration_trace_id),
                                Some(task.agent_id),
                                Some(task.id),
                            )
                            .await;
                        }
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
                if action == BudgetAction::Suspend {
                    match self
                        .scheduler
                        .update_state_if_not_terminal(&task.id, TaskState::Suspended)
                        .await
                    {
                        Ok(true) => {
                            self.emit_event_with_trace(
                                EventType::TaskSuspended,
                                EventSource::TaskScheduler,
                                EventSeverity::Warning,
                                serde_json::json!({
                                    "task_id": task.id.to_string(),
                                    "agent_id": task.agent_id.to_string(),
                                    "resource": resource,
                                    "reason": "budget_hard_limit_suspend_pre_inference",
                                }),
                                0,
                                Some(iteration_trace_id),
                                Some(task.agent_id),
                                Some(task.id),
                            )
                            .await;
                            anyhow::bail!(
                                "task suspended: budget hard limit reached: {}",
                                resource
                            );
                        }
                        Ok(false) => {
                            tracing::warn!(
                                task_id = %task.id,
                                "Budget suspension (pre-inference): task already terminal"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                task_id = %task.id,
                                error = %e,
                                "Failed to set task to Suspended during pre-inference budget enforcement"
                            );
                        }
                    }
                }
                return Err(anyhow::Error::new(AgentOSError::BudgetExceeded {
                    agent_id: task.agent_id.to_string(),
                    detail: format!("hard limit exceeded (pre-inference): {}", resource),
                }));
            }

            tracing::info!("Task {} iteration {}: calling LLM", task.id, iteration);

            let inference = match current_llm
                .infer_with_tools(&compiled_context, &llm_tool_manifests)
                .await
            {
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
            tracing::debug!(
                task_id = %task.id,
                iteration = iteration,
                tokens = inference.tokens_used.total_tokens,
                duration_ms = inference.duration_ms,
                output = %inference.text,
                "LLM raw output"
            );

            // --- Cost budget enforcement ---
            let budget_result = self
                .cost_tracker
                .record_inference_with_cost(
                    &task.agent_id,
                    &inference.tokens_used,
                    current_llm.provider_name(),
                    current_llm.model_name(),
                    inference.cost.as_ref(),
                )
                .await;

            // --- Structured cost attribution audit entry (Spec §4) ---
            if let Some(snapshot) = self.cost_tracker.get_snapshot(&task.agent_id).await {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    // Use task-level trace_id so CostAttribution can be correlated
                    // with TaskStarted/TaskFailed by trace. Include iteration_trace_id
                    // in details for finer-grained per-inference correlation.
                    trace_id: *task_trace_id,
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
                        "iteration_trace_id": iteration_trace_id.to_string(),
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
                    self.emit_event_with_trace(
                        EventType::BudgetWarning,
                        EventSource::InferenceKernel,
                        EventSeverity::Warning,
                        serde_json::json!({
                            "task_id": task.id.to_string(),
                            "agent_id": task.agent_id.to_string(),
                            "resource": resource,
                            "usage_pct": current_pct,
                        }),
                        task.trigger_source
                            .as_ref()
                            .map(|ts| ts.chain_depth + 1)
                            .unwrap_or(0),
                        Some(iteration_trace_id),
                        Some(task.agent_id),
                        Some(task.id),
                    )
                    .await;
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
                    self.emit_event_with_trace(
                        EventType::BudgetExhausted,
                        EventSource::InferenceKernel,
                        EventSeverity::Warning,
                        serde_json::json!({
                            "task_id": task.id.to_string(),
                            "agent_id": task.agent_id.to_string(),
                            "resource": resource,
                            "action": "pause",
                            "usage_pct": current_pct,
                        }),
                        task.trigger_source
                            .as_ref()
                            .map(|ts| ts.chain_depth + 1)
                            .unwrap_or(0),
                        Some(iteration_trace_id),
                        Some(task.agent_id),
                        Some(task.id),
                    )
                    .await;
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
                    self.emit_event_with_trace(
                        EventType::BudgetExhausted,
                        EventSource::InferenceKernel,
                        EventSeverity::Critical,
                        serde_json::json!({
                            "task_id": task.id.to_string(),
                            "agent_id": task.agent_id.to_string(),
                            "resource": resource,
                            "action": format!("{:?}", action),
                        }),
                        task.trigger_source
                            .as_ref()
                            .map(|ts| ts.chain_depth + 1)
                            .unwrap_or(0),
                        Some(iteration_trace_id),
                        Some(task.agent_id),
                        Some(task.id),
                    )
                    .await;
                    self.context_manager.remove_context(&task.id).await;
                    self.intent_validator.remove_task(&task.id).await;
                    if *action == BudgetAction::Suspend {
                        match self
                            .scheduler
                            .update_state_if_not_terminal(&task.id, TaskState::Suspended)
                            .await
                        {
                            Ok(true) => {
                                self.emit_event_with_trace(
                                    EventType::TaskSuspended,
                                    EventSource::TaskScheduler,
                                    EventSeverity::Warning,
                                    serde_json::json!({
                                        "task_id": task.id.to_string(),
                                        "agent_id": task.agent_id.to_string(),
                                        "resource": resource,
                                        "reason": "budget_hard_limit_suspend",
                                    }),
                                    0,
                                    Some(iteration_trace_id),
                                    Some(task.agent_id),
                                    Some(task.id),
                                )
                                .await;
                                anyhow::bail!(
                                    "task suspended: budget hard limit reached: {}",
                                    resource
                                );
                            }
                            Ok(false) => {
                                tracing::warn!(
                                    task_id = %task.id,
                                    "Budget suspension (post-inference): task already terminal"
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    task_id = %task.id,
                                    error = %e,
                                    "Failed to set task to Suspended during post-inference budget enforcement"
                                );
                            }
                        }
                    }
                    return Err(anyhow::Error::new(AgentOSError::BudgetExceeded {
                        agent_id: task.agent_id.to_string(),
                        detail: format!("hard limit exceeded: {}", resource),
                    }));
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

            // Push assistant response into context, preserving tool_calls so
            // adapters can reconstruct the provider-native format on the next turn.
            let assistant_tool_calls_json = if inference.tool_calls.is_empty() {
                None
            } else {
                match serde_json::to_value(&inference.tool_calls) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::error!(
                            task_id = %task.id,
                            error = %e,
                            "Failed to serialize tool_calls into context metadata — \
                             multi-turn tool protocol will break on next inference"
                        );
                        None
                    }
                }
            };
            if let Err(e) = self
                .context_manager
                .push_entry(
                    &task.id,
                    ContextEntry {
                        role: ContextRole::Assistant,
                        content: inference.text.clone(),
                        timestamp: chrono::Utc::now(),
                        metadata: Some(ContextMetadata {
                            tool_name: None,
                            tool_id: None,
                            intent_id: None,
                            tokens_estimated: None,
                            tool_call_id: None,
                            assistant_tool_calls: assistant_tool_calls_json,
                        }),
                        importance: 0.4,
                        pinned: false,
                        reference_count: 0,
                        partition: ContextPartition::default(),
                        category: ContextCategory::History,
                        is_summary: false,
                    },
                )
                .await
            {
                tracing::warn!(task_id = %task.id, error = %e, "Failed to push assistant response to context window");
            }

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
                .await
            {
                tracing::warn!(task_id = %task.id, error = %e, "Failed to record episodic memory");
            }

            // Capture any [FEEDBACK]...[/FEEDBACK] blocks emitted by the agent and
            // record each as a TestFindingCaptured audit event so the web UI can
            // surface them in real-time via the task log SSE stream.
            for finding in extract_feedback_blocks(&inference.text) {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TestFindingCaptured,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: finding,
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
            }

            // Prefer native tool calls from the adapter. Use tool_calls presence
            // as the primary signal; StopReason is supplementary.
            // Fallback: if the adapter returned no structured tool calls, try to parse
            // a JSON tool call from the plain text response (for models without native
            // function-calling support, e.g. some Ollama models).
            let mut parsed_tool_calls: Vec<crate::tool_call::ToolCallRequest> = inference
                .tool_calls
                .iter()
                .map(|tc| crate::tool_call::ToolCallRequest {
                    id: tc.id.clone(),
                    tool_name: tc.tool_name.clone(),
                    intent_type: crate::tool_call::parse_intent_type(&tc.intent_type)
                        .unwrap_or(IntentType::Query),
                    payload: tc.payload.clone(),
                })
                .collect();
            if parsed_tool_calls.is_empty() {
                if let Some(text_tc) = crate::tool_call::parse_tool_call_from_text(&inference.text)
                {
                    tracing::info!(
                        task_id = %task.id,
                        tool = %text_tc.tool_name,
                        "Parsed text-mode tool call from LLM response (no native function calling)"
                    );
                    parsed_tool_calls.push(text_tc);
                }
            }
            if parsed_tool_calls.len() > 1 {
                if self.config.kernel.tool_calls.allow_parallel {
                    self.execute_parallel_tool_calls(
                        task,
                        task_trace_id,
                        iteration,
                        parsed_tool_calls,
                        &mut tool_call_count,
                        &mut refresh_knowledge_blocks,
                    )
                    .await?;
                    continue;
                } else {
                    tracing::warn!(
                        task_id = %task.id,
                        total_calls = parsed_tool_calls.len(),
                        "Parallel tool calls disabled; executing only the first call"
                    );
                }
            }

            // Check for a single tool call (reuse already-parsed result)
            match parsed_tool_calls.into_iter().next() {
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
                                if let Err(e) = self
                                    .context_manager
                                    .push_tool_result(
                                        &task.id,
                                        &tool_call.tool_name,
                                        &error_result,
                                        tool_call.id.clone(),
                                    )
                                    .await
                                {
                                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                                }
                                continue;
                            }
                            Ok(IntentCoherenceResult::Rejected { reason }) => {
                                let error_result = serde_json::json!({
                                    "error": format!("Coherence check failed: {}", reason)
                                });
                                if let Err(e) = self
                                    .context_manager
                                    .push_tool_result(
                                        &task.id,
                                        &tool_call.tool_name,
                                        &error_result,
                                        tool_call.id.clone(),
                                    )
                                    .await
                                {
                                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                                }
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
                        if let Err(e) = self
                            .context_manager
                            .push_tool_result(
                                &task.id,
                                &tool_call.tool_name,
                                &context_result,
                                tool_call.id.clone(),
                            )
                            .await
                        {
                            tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                        }
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
                                    Some(task.agent_id),
                                    Some(task.id),
                                )
                                .await;

                                let error_result = serde_json::json!({
                                    "error": format!("Unknown tool requested: {}", tool_call.tool_name)
                                });
                                if let Err(e) = self
                                    .context_manager
                                    .push_tool_result(
                                        &task.id,
                                        &tool_call.tool_name,
                                        &error_result,
                                        tool_call.id.clone(),
                                    )
                                    .await
                                {
                                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                                }
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
                                    Some(task.agent_id),
                                    Some(task.id),
                                )
                                .await;

                                let error_result = serde_json::json!({
                                    "error": format!("Unauthorized tool access blocked: {}", tool_call.tool_name)
                                });
                                if let Err(e) = self
                                    .context_manager
                                    .push_tool_result(
                                        &task.id,
                                        &tool_call.tool_name,
                                        &error_result,
                                        tool_call.id.clone(),
                                    )
                                    .await
                                {
                                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                                }
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
                                Some(task.agent_id),
                                Some(task.id),
                            )
                            .await;

                            let error_result = serde_json::json!({
                                "error": format!("Permission denied: {}", denial_reason)
                            });
                            if let Err(e) = self
                                .context_manager
                                .push_tool_result(
                                    &task.id,
                                    &tool_call.tool_name,
                                    &error_result,
                                    tool_call.id.clone(),
                                )
                                .await
                            {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            }
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
                            if let Err(e) = self
                                .context_manager
                                .push_tool_result(
                                    &task.id,
                                    &tool_call.tool_name,
                                    &error_result,
                                    tool_call.id.clone(),
                                )
                                .await
                            {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            }
                            continue;
                        }
                        Ok(IntentCoherenceResult::Suspicious { reason, .. }) => {
                            // Inject loop warning so the LLM knows it is repeating itself
                            let warning = serde_json::json!({
                                "warning": format!("LOOP DETECTED: {}. You are repeating the same action. Try a different approach or complete the task with the information you already have.", reason)
                            });
                            if let Err(e) = self
                                .context_manager
                                .push_tool_result(
                                    &task.id,
                                    &tool_call.tool_name,
                                    &warning,
                                    tool_call.id.clone(),
                                )
                                .await
                            {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            }
                        }
                        Ok(IntentCoherenceResult::Approved) => {
                            // All clear
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
                        if *action == BudgetAction::Suspend {
                            match self
                                .scheduler
                                .update_state_if_not_terminal(&task.id, TaskState::Suspended)
                                .await
                            {
                                Ok(true) => {
                                    self.emit_event_with_trace(
                                        EventType::TaskSuspended,
                                        EventSource::TaskScheduler,
                                        EventSeverity::Warning,
                                        serde_json::json!({
                                            "task_id": task.id.to_string(),
                                            "agent_id": task.agent_id.to_string(),
                                            "resource": resource,
                                            "reason": "budget_tool_call_limit_suspend",
                                        }),
                                        0,
                                        Some(trace_id),
                                        Some(task.agent_id),
                                        Some(task.id),
                                    )
                                    .await;
                                    anyhow::bail!(
                                        "task suspended: tool call budget hard limit reached: {}",
                                        resource
                                    );
                                }
                                Ok(false) => {
                                    tracing::warn!(
                                        task_id = %task.id,
                                        "Budget suspension (tool-call): task already terminal"
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        task_id = %task.id,
                                        error = %e,
                                        "Failed to set task to Suspended during tool-call budget enforcement"
                                    );
                                }
                            }
                        }
                        return Err(anyhow::Error::new(AgentOSError::BudgetExceeded {
                            agent_id: task.agent_id.to_string(),
                            detail: format!("tool call hard limit exceeded: {}", resource),
                        }));
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
                            if let Err(e) = self
                                .context_manager
                                .push_tool_result(
                                    &task.id,
                                    &tool_call.tool_name,
                                    &error_result,
                                    tool_call.id.clone(),
                                )
                                .await
                            {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            }
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
                            if let Err(e) = self
                                .scheduler
                                .update_state(&task.id, TaskState::Waiting)
                                .await
                            {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to update task state to Waiting — task may be stuck in Running state");
                            }
                            let waiting_result = serde_json::json!({
                                "status": "awaiting_approval",
                                "message": "This action requires human approval. Task is paused."
                            });
                            if let Err(e) = self
                                .context_manager
                                .push_tool_result(
                                    &task.id,
                                    &tool_call.tool_name,
                                    &waiting_result,
                                    tool_call.id.clone(),
                                )
                                .await
                            {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            }
                            // Preserve context and intent history so the agent
                            // can resume with full state when approval arrives.
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

                    if let Err(e) = self
                        .episodic_memory
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
                        .await
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

                    // Build lightweight snapshots for agent-list / task-status / task-list tools.
                    let agent_snapshot = {
                        let registry = self.agent_registry.read().await;
                        let agents: Vec<AgentSummary> = registry
                            .list_all()
                            .into_iter()
                            .map(|p| AgentSummary {
                                id: p.id,
                                name: p.name.clone(),
                                status: format!("{:?}", p.status).to_lowercase(),
                                registered_at: p.created_at,
                            })
                            .collect();
                        AgentRegistrySnapshot::new(agents)
                    };
                    let task_snapshot = self.scheduler.snapshot_tasks().await;
                    let escalation_snapshot = {
                        let pending = self.escalation_manager.list_pending().await;
                        let agent_id = task.agent_id;
                        let summaries: Vec<EscalationSummary> = pending
                            .into_iter()
                            .filter(|e| e.agent_id == agent_id)
                            .map(|e| EscalationSummary {
                                id: e.id,
                                task_id: e.task_id,
                                agent_id: e.agent_id,
                                reason: format!("{:?}", e.reason),
                                context_summary: e.context_summary,
                                decision_point: e.decision_point,
                                options: e.options,
                                urgency: e.urgency,
                                blocking: e.blocking,
                                created_at: e.created_at,
                                expires_at: e.expires_at,
                                resolved: e.resolved,
                                resolution: e.resolution,
                            })
                            .collect();
                        EscalationSnapshot::new(summaries)
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
                        file_lock_registry: None,
                        agent_registry: Some(
                            Arc::new(agent_snapshot) as Arc<dyn AgentRegistryQuery>
                        ),
                        task_registry: Some(Arc::new(task_snapshot) as Arc<dyn TaskQuery>),
                        escalation_query: Some(
                            Arc::new(escalation_snapshot) as Arc<dyn EscalationQuery>
                        ),
                        workspace_paths: self.workspace_paths.clone(),
                        cancellation_token: self.cancellation_token.child_token(),
                    };
                    let tool_payload_preview = Self::truncate_for_prompt_payload(
                        &serde_json::to_string(&tool_call.payload).unwrap_or_default(),
                        600,
                    );

                    let tool_start = std::time::Instant::now();
                    let sandbox_plan = self.sandbox_plan_for_tool(&tool_call.tool_name).await;
                    let execution_mode: &'static str = if sandbox_plan.is_some() {
                        "sandbox"
                    } else {
                        "in_process"
                    };

                    self.emit_event_with_trace(
                        EventType::ToolCallStarted,
                        EventSource::ToolRunner,
                        EventSeverity::Info,
                        serde_json::json!({
                            "tool_name": tool_call.tool_name,
                            "task_id": task.id.to_string(),
                            "agent_id": task.agent_id.to_string(),
                            "execution_mode": execution_mode,
                        }),
                        task.trigger_source
                            .as_ref()
                            .map(|ts| ts.chain_depth + 1)
                            .unwrap_or(0),
                        Some(trace_id),
                        Some(task.agent_id),
                        Some(task.id),
                    )
                    .await;

                    let tool_result = {
                        if let Some((config, category_overhead_bytes, manifest_weight)) =
                            sandbox_plan
                        {
                            let timeout = Duration::from_millis(config.max_cpu_ms.max(5000));
                            let request = SandboxExecRequest {
                                tool_name: tool_call.tool_name.clone(),
                                payload: tool_call.payload.clone(),
                                data_dir: exec_context.data_dir.clone(),
                                manifest_weight,
                                task_id: Some(exec_context.task_id),
                                agent_id: Some(exec_context.agent_id),
                                trace_id: Some(exec_context.trace_id),
                                permissions: exec_context.permissions.clone(),
                                workspace_paths: Some(exec_context.workspace_paths.clone()),
                            };
                            match self
                                .sandbox
                                .spawn(request, &config, timeout, category_overhead_bytes)
                                .await
                            {
                                Ok(sandbox_result) => {
                                    SandboxExecutor::parse_result(&sandbox_result)
                                }
                                Err(e) => {
                                    tracing::error!(
                                        tool = %tool_call.tool_name,
                                        error = %e,
                                        "Sandbox spawn failed — refusing unsandboxed execution"
                                    );
                                    Err(e)
                                }
                            }
                        } else {
                            let timeout_secs =
                                self.config.kernel.tool_execution.default_timeout_seconds;
                            match tokio::time::timeout(
                                Duration::from_secs(timeout_secs),
                                self.tool_runner.execute(
                                    &tool_call.tool_name,
                                    tool_call.payload,
                                    exec_context,
                                ),
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(_) => {
                                    tracing::warn!(
                                        tool = %tool_call.tool_name,
                                        timeout_secs,
                                        "In-process tool call timed out"
                                    );
                                    Err(agentos_types::AgentOSError::ToolExecutionFailed {
                                        tool_name: tool_call.tool_name.clone(),
                                        reason: format!("timed out after {}s", timeout_secs),
                                    })
                                }
                            }
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
                            {
                                let chain_depth = task
                                    .trigger_source
                                    .as_ref()
                                    .map(|ts| ts.chain_depth + 1)
                                    .unwrap_or(0);
                                self.emit_event_with_trace(
                                    EventType::ToolCallCompleted,
                                    EventSource::ToolRunner,
                                    EventSeverity::Info,
                                    serde_json::json!({
                                        "tool_name": tool_call.tool_name,
                                        "task_id": task.id.to_string(),
                                        "agent_id": task.agent_id.to_string(),
                                        "duration_ms": tool_start.elapsed().as_millis() as u64,
                                        "execution_mode": execution_mode,
                                    }),
                                    chain_depth,
                                    Some(trace_id),
                                    Some(task.agent_id),
                                    Some(task.id),
                                )
                                .await;
                            }

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
                            let result_str = Self::maybe_truncate_output(
                                context_result.to_string(),
                                self.config.kernel.tool_execution.max_output_bytes,
                                &tool_call.tool_name,
                            );
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
                                                                Some(task.agent_id),
                                Some(task.id),
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
                                    if let Err(e) = self
                                        .scheduler
                                        .update_state(&task.id, TaskState::Waiting)
                                        .await
                                    {
                                        tracing::error!(error = %e, task_id = %task.id, "Failed to update task state to Waiting — task may be stuck in Running state");
                                    }
                                    // Preserve both context and intent history so the task
                                    // can resume with full state if the escalation is approved.
                                    // The tainted output is never pushed to context (bail
                                    // happens before push_tool_result).
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

                            match self
                                .context_manager
                                .push_tool_result(
                                    &task.id,
                                    &tool_call.tool_name,
                                    &tainted_result,
                                    tool_call.id.clone(),
                                )
                                .await
                            {
                                Ok(evicted) => {
                                    consecutive_push_failures = 0;
                                    if evicted > 0 {
                                        let chain_depth = task
                                            .trigger_source
                                            .as_ref()
                                            .map(|ts| ts.chain_depth + 1)
                                            .unwrap_or(0);
                                        self.emit_event_with_trace(
                                            EventType::WorkingMemoryEviction,
                                            EventSource::ContextManager,
                                            EventSeverity::Info,
                                            serde_json::json!({
                                                "task_id": task.id.to_string(),
                                                "agent_id": task.agent_id.to_string(),
                                                "entries_evicted": evicted,
                                            }),
                                            chain_depth,
                                            Some(trace_id),
                                            Some(task.agent_id),
                                            Some(task.id),
                                        )
                                        .await;
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                                    consecutive_push_failures += 1;
                                    if consecutive_push_failures >= 3 {
                                        anyhow::bail!("Task aborted: {} consecutive context push failures — agent context is unreliable", consecutive_push_failures);
                                    }
                                }
                            }

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

                            if let Err(e) = self
                                .episodic_memory
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
                                .await
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
                                    "execution_mode": execution_mode,
                                }),
                                chain_depth,
                                Some(trace_id),
                                Some(task.agent_id),
                                Some(task.id),
                            )
                            .await;

                            // Detect sandbox violations and emit security events
                            let error_msg = e.to_string().to_lowercase();
                            if error_msg.contains("sandbox")
                                || error_msg.contains("seccomp")
                                || error_msg.contains("syscall denied")
                            {
                                self.emit_event_with_trace(
                                    EventType::SandboxEscapeAttempt,
                                    EventSource::SecurityEngine,
                                    EventSeverity::Critical,
                                    serde_json::json!({
                                        "task_id": task.id.to_string(),
                                        "agent_id": task.agent_id.to_string(),
                                        "tool_name": tool_call.tool_name,
                                        "violation": e.to_string(),
                                    }),
                                    chain_depth,
                                    Some(trace_id),
                                    Some(task.agent_id),
                                    Some(task.id),
                                )
                                .await;
                                self.emit_event_with_trace(
                                    EventType::ToolSandboxViolation,
                                    EventSource::ToolRunner,
                                    EventSeverity::Critical,
                                    serde_json::json!({
                                        "task_id": task.id.to_string(),
                                        "agent_id": task.agent_id.to_string(),
                                        "tool_name": tool_call.tool_name,
                                        "violation": e.to_string(),
                                    }),
                                    chain_depth,
                                    Some(trace_id),
                                    Some(task.agent_id),
                                    Some(task.id),
                                )
                                .await;
                            }

                            // Detect resource quota violations
                            if error_msg.contains("resource")
                                || error_msg.contains("quota")
                                || error_msg.contains("memory limit")
                                || error_msg.contains("cpu limit")
                                || error_msg.contains("oom")
                            {
                                self.emit_event_with_trace(
                                    EventType::ToolResourceQuotaExceeded,
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
                                    Some(task.agent_id),
                                    Some(task.id),
                                )
                                .await;
                            }

                            let error_result = serde_json::json!({
                                "error": e.to_string()
                            });
                            if let Err(e) = self
                                .context_manager
                                .push_tool_result(
                                    &task.id,
                                    &tool_call.tool_name,
                                    &error_result,
                                    tool_call.id.clone(),
                                )
                                .await
                            {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            }

                            if let Err(record_err) = self
                                .episodic_memory
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
                                .await
                            {
                                tracing::warn!(task_id = %task.id, error = %record_err, "Failed to record episodic memory");
                            }
                        }
                    }
                }
                None => {
                    // No tool call — LLM produced a plain text response.
                    // Only re-prompt if tools are actually available; short answers
                    // are valid when no tools exist (e.g. pure Q&A tasks).
                    if iteration == 0 && inference.text.len() < 20 && !llm_tool_manifests.is_empty()
                    {
                        tracing::warn!(
                            task_id = %task.id,
                            text_len = inference.text.len(),
                            "First iteration short response — re-prompting agent to use tools"
                        );
                        // Push a re-prompt and give the agent another iteration
                        let reprompt = "Your previous response was too short and contained no tool calls. \
                            Please use the available tools to accomplish your task, or provide a substantive answer.";
                        if let Err(e) = self
                            .context_manager
                            .push_entry(
                                &task.id,
                                agentos_types::ContextEntry {
                                    role: agentos_types::ContextRole::System,
                                    content: reprompt.to_string(),
                                    timestamp: chrono::Utc::now(),
                                    metadata: None,
                                    importance: 0.9,
                                    pinned: false,
                                    reference_count: 0,
                                    partition: agentos_types::ContextPartition::default(),
                                    category: agentos_types::ContextCategory::Task,
                                    is_summary: false,
                                },
                            )
                            .await
                        {
                            tracing::warn!(error = %e, "Failed to push re-prompt — accepting short answer");
                            final_answer = inference.text;
                            break;
                        }
                        // Continue to next iteration instead of breaking
                        continue;
                    }
                    final_answer = inference.text;
                    break;
                }
            }
        }

        if final_answer.is_empty() {
            if completed_iterations >= max_iterations {
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
    #[tracing::instrument(skip_all, fields(task_id = %task.id, agent_id = %task.agent_id))]
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
        if let Err(e) = self.scheduler.mark_started(&task.id).await {
            tracing::error!(error = %e, task_id = %task.id, "Failed to mark task as started in scheduler");
        }

        self.push_status_update(task.id, TaskState::Running, "Task started".to_string());

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: task_trace_id,
            event_type: agentos_audit::AuditEventType::TaskCreated,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "prompt_preview": task.original_prompt.chars().take(200).collect::<String>(),
                "autonomous": task.autonomous,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

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
            Some(task.agent_id),
            Some(task.id),
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
            .list_online()
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

    /// Truncate tool output before it enters the context window.
    ///
    /// Only the size injected into the agent's context is capped — the tool ran
    /// to completion and the task loop continues unchanged. The truncation marker
    /// tells the agent it received partial output so it can request smaller chunks
    /// or use a different approach. This never terminates an agentic workflow.
    pub(crate) fn maybe_truncate_output(s: String, max_bytes: usize, tool_name: &str) -> String {
        if s.len() <= max_bytes {
            return s;
        }
        let original_len = s.len();
        // Truncate at a char boundary at or before max_bytes.
        let truncated: String = s
            .char_indices()
            .take_while(|(idx, _)| *idx < max_bytes)
            .map(|(_, c)| c)
            .collect();
        tracing::warn!(
            tool = %tool_name,
            original_bytes = original_len,
            limit_bytes = max_bytes,
            "Tool output truncated before context injection"
        );
        format!(
            "{} [TRUNCATED: output was {} bytes, limit {} bytes — request smaller data or use pagination]",
            truncated, original_len, max_bytes
        )
    }
}

/// Parse `[FEEDBACK]...[/FEEDBACK]` blocks from an LLM response.
/// Each block must contain a valid JSON object. Malformed blocks are silently skipped.
/// Used to surface structured agent feedback as `TestFindingCaptured` audit events.
fn extract_feedback_blocks(text: &str) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find("[FEEDBACK]") {
        let abs_start = search_from + start + "[FEEDBACK]".len();
        if let Some(end_offset) = text[abs_start..].find("[/FEEDBACK]") {
            let block = text[abs_start..abs_start + end_offset].trim();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(block) {
                results.push(val);
            }
            search_from = abs_start + end_offset + "[/FEEDBACK]".len();
        } else {
            break;
        }
    }
    results
}

/// Determine whether a tool should be sandboxed based on policy and trust tier.
///
/// Extracted as a pure function for testability — the full `sandbox_plan_for_tool()`
/// method requires a running kernel with tool registry access.
fn should_sandbox_tool(policy: crate::config::SandboxPolicy, trust_tier: TrustTier) -> bool {
    match policy {
        crate::config::SandboxPolicy::Never => false,
        crate::config::SandboxPolicy::Always => true,
        crate::config::SandboxPolicy::TrustAware => trust_tier != TrustTier::Core,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

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

    fn make_task(complexity: Option<ComplexityLevel>, max_iterations: Option<u32>) -> AgentTask {
        AgentTask {
            id: TaskID::new(),
            state: TaskState::Queued,
            agent_id: AgentID::new(),
            capability_token: CapabilityToken {
                task_id: TaskID::new(),
                agent_id: AgentID::new(),
                allowed_tools: BTreeSet::new(),
                allowed_intents: BTreeSet::new(),
                permissions: PermissionSet::new(),
                issued_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now(),
                signature: Vec::new(),
            },
            assigned_llm: None,
            priority: 5,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(60),
            original_prompt: "test".to_string(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: complexity.map(|estimated_complexity| TaskReasoningHints {
                estimated_complexity,
                preferred_turns: None,
                preemption_sensitivity: PreemptionLevel::Normal,
            }),
            max_iterations,
            trigger_source: None,
            autonomous: false,
        }
    }

    fn default_autonomous_config() -> crate::config::AutonomousModeConfig {
        crate::config::AutonomousModeConfig::default()
    }

    #[test]
    fn resolve_task_max_iterations_uses_per_task_override() {
        let limits = crate::config::TaskLimitsConfig {
            max_iterations_low: 10,
            max_iterations_medium: 25,
            max_iterations_high: 50,
        };
        let task = make_task(Some(ComplexityLevel::High), Some(7));

        assert_eq!(
            Kernel::resolve_task_max_iterations(&task, &limits, &default_autonomous_config()),
            7
        );
    }

    #[test]
    fn resolve_task_max_iterations_uses_complexity_defaults() {
        let limits = crate::config::TaskLimitsConfig {
            max_iterations_low: 9,
            max_iterations_medium: 21,
            max_iterations_high: 55,
        };

        assert_eq!(
            Kernel::resolve_task_max_iterations(
                &make_task(Some(ComplexityLevel::Low), None),
                &limits,
                &default_autonomous_config()
            ),
            9
        );
        assert_eq!(
            Kernel::resolve_task_max_iterations(
                &make_task(Some(ComplexityLevel::Medium), None),
                &limits,
                &default_autonomous_config()
            ),
            21
        );
        assert_eq!(
            Kernel::resolve_task_max_iterations(
                &make_task(Some(ComplexityLevel::High), None),
                &limits,
                &default_autonomous_config()
            ),
            55
        );
    }

    #[test]
    fn resolve_task_max_iterations_defaults_to_low_without_hints() {
        let limits = crate::config::TaskLimitsConfig {
            max_iterations_low: 12,
            max_iterations_medium: 24,
            max_iterations_high: 48,
        };

        assert_eq!(
            Kernel::resolve_task_max_iterations(
                &make_task(None, None),
                &limits,
                &default_autonomous_config()
            ),
            12
        );
    }

    #[test]
    fn resolve_task_max_iterations_clamps_zero_to_one() {
        let limits = crate::config::TaskLimitsConfig {
            max_iterations_low: 0,
            max_iterations_medium: 25,
            max_iterations_high: 50,
        };
        // Zero in config should be clamped to 1.
        assert_eq!(
            Kernel::resolve_task_max_iterations(
                &make_task(None, None),
                &limits,
                &default_autonomous_config()
            ),
            1
        );
        // Zero as per-task override should also be clamped to 1.
        assert_eq!(
            Kernel::resolve_task_max_iterations(
                &make_task(Some(ComplexityLevel::High), Some(0)),
                &limits,
                &default_autonomous_config()
            ),
            1
        );
    }

    #[test]
    fn maybe_truncate_output_passes_through_small_output() {
        let s = "hello world".to_string();
        let result = Kernel::maybe_truncate_output(s.clone(), 1024, "test-tool");
        assert_eq!(result, s);
    }

    #[test]
    fn maybe_truncate_output_truncates_large_output() {
        let s = "x".repeat(512 * 1024); // 512 KiB
        let limit = 256 * 1024; // 256 KiB
        let result = Kernel::maybe_truncate_output(s, limit, "big-tool");
        assert!(result.len() > limit); // includes the marker suffix
        assert!(result.contains("[TRUNCATED:"));
        assert!(result.contains("524288 bytes")); // original size
        assert!(result.contains("262144 bytes")); // limit
                                                  // Actual content prefix must be exactly at the limit
        let content_len = result.find(" [TRUNCATED:").unwrap();
        assert_eq!(content_len, limit);
    }

    #[test]
    fn maybe_truncate_output_handles_exact_limit() {
        let s = "a".repeat(256);
        let result = Kernel::maybe_truncate_output(s.clone(), 256, "tool");
        assert_eq!(result, s); // no truncation at exact limit
    }

    #[test]
    fn trust_aware_core_runs_in_process() {
        assert!(!should_sandbox_tool(
            crate::config::SandboxPolicy::TrustAware,
            TrustTier::Core
        ));
    }

    #[test]
    fn trust_aware_verified_sandboxed() {
        assert!(should_sandbox_tool(
            crate::config::SandboxPolicy::TrustAware,
            TrustTier::Verified
        ));
    }

    #[test]
    fn trust_aware_community_sandboxed() {
        assert!(should_sandbox_tool(
            crate::config::SandboxPolicy::TrustAware,
            TrustTier::Community
        ));
    }

    #[test]
    fn always_sandboxes_core() {
        assert!(should_sandbox_tool(
            crate::config::SandboxPolicy::Always,
            TrustTier::Core
        ));
    }

    #[test]
    fn never_skips_sandbox_for_community() {
        assert!(!should_sandbox_tool(
            crate::config::SandboxPolicy::Never,
            TrustTier::Community
        ));
    }

    #[test]
    fn never_skips_sandbox_for_verified() {
        assert!(!should_sandbox_tool(
            crate::config::SandboxPolicy::Never,
            TrustTier::Verified
        ));
    }

    #[test]
    fn trust_aware_blocked_would_sandbox() {
        // Blocked tools are rejected earlier at registration, but if they
        // somehow reach dispatch, they should be treated as untrusted.
        assert!(should_sandbox_tool(
            crate::config::SandboxPolicy::TrustAware,
            TrustTier::Blocked
        ));
    }
}
