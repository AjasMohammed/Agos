use crate::context_compactor::ContextCompactor;
use crate::escalation::{EscalationManager, ResolutionOutcome};
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

/// Maximum wall-clock time `task_executor` will park waiting for human
/// resolution of a privileged-tool escalation. Set just above the
/// default `EscalationManager` timeout (5 min) so the manager's auto-
/// deny sweeper wins the race in the common case.
const APPROVAL_WAIT_TIMEOUT_SECS: u64 = 360;

/// Outcome returned by [`wait_for_approval_resolution`] — a small
/// classifier so the caller can pick a typed `AgentOSError` reason
/// without inspecting the receiver state machine.
enum ApprovalWaitOutcome {
    Approved,
    Denied,
    /// Receiver was missing or the sender was dropped without a value
    /// (race with concurrent resolve / kernel restart). Treated as a
    /// failure but with a distinct reason string so operators can grep
    /// audit logs for stuck approvals.
    Lost,
}

/// Parse an `approval_pending:<id>:...` abort reason produced by
/// `ApprovalHook::on_event`. Returns `Some(id)` for the structured
/// shape; any other reason returns `None` and is treated as a hard
/// hook denial.
fn extract_approval_pending_id(reason: &str) -> Option<u64> {
    let rest = reason.strip_prefix("approval_pending:")?;
    let id_str = rest.split(':').next()?;
    id_str.trim().parse::<u64>().ok()
}

/// Park on the resolution channel for `escalation_id` with a hard
/// upper-bound timeout. Returns [`ApprovalWaitOutcome::Lost`] if the
/// channel is missing or already taken — the caller will surface a
/// typed tool failure so the agent can retry.
async fn wait_for_approval_resolution(
    escalation_manager: Arc<EscalationManager>,
    escalation_id: u64,
) -> ApprovalWaitOutcome {
    let Some(rx) = escalation_manager
        .take_resolution_receiver(escalation_id)
        .await
    else {
        return ApprovalWaitOutcome::Lost;
    };
    match tokio::time::timeout(Duration::from_secs(APPROVAL_WAIT_TIMEOUT_SECS), rx).await {
        Ok(Ok(ResolutionOutcome::Approved)) => ApprovalWaitOutcome::Approved,
        Ok(Ok(ResolutionOutcome::Denied)) => ApprovalWaitOutcome::Denied,
        Ok(Err(_)) => ApprovalWaitOutcome::Lost,
        Err(_) => ApprovalWaitOutcome::Denied,
    }
}
use tracing::Instrument;

/// Soft threshold (seconds) after which a long-running LLM inference is
/// escalated to the user instead of being killed outright. The previous
/// hard timeout sat here; with the user-gate in place the inference is
/// allowed to keep running while we ask the user whether to abort.
const LLM_INFERENCE_TIMEOUT_SECS: u64 = 120;

/// Time (seconds) we wait for the user to respond to a long-running
/// inference escalation before defaulting to abort. Kept short so a
/// stuck task is not unbounded when the user is offline.
const LLM_INFERENCE_USER_GRACE_SECS: u64 = 60;

/// Maximum number of times the user can extend a single inference
/// before we force-abort. Upper bound on total wall-clock per inference is
/// `(MAX_EXTENSIONS + 1) * LLM_INFERENCE_TIMEOUT_SECS + MAX_EXTENSIONS * LLM_INFERENCE_USER_GRACE_SECS`
/// — with the defaults that's 4*120 + 3*60 = 660s ≈ 11 min, since the
/// inference keeps running while we wait for the user during each grace window.
const LLM_INFERENCE_MAX_EXTENSIONS: u32 = 3;

/// Outcome of one watchdog tick: either the inference finished, or the
/// soft threshold elapsed and we need to ask the user.
enum InferenceWatchdogStep<T> {
    Completed(T),
    Threshold,
}

/// Outcome of the user-gate race: either the inference finished while
/// we were waiting on the user, the user picked Continue, or we should
/// abort (user denied / grace expired / channel lost).
enum InferenceGateStep<T> {
    Completed(T),
    Continue,
    Abort,
}

/// Result of synchronous task execution, carrying data needed by the outer
/// `execute_task()` method for enriched episodic memory recording.
pub(crate) struct TaskResult {
    pub answer: String,
    pub tool_call_count: u32,
    pub iterations: u32,
    /// Per-tool records for downstream observability (run history,
    /// delivery context). Populated incrementally during execution.
    pub tool_calls: Vec<agentos_types::ToolCallRecord>,
}

impl Kernel {
    fn manual_query_details(
        tool_name: &str,
        payload: &serde_json::Value,
        result: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        let (layer, section, category, page) = match tool_name {
            "list-tools" => (
                "L1",
                Some("tools".to_string()),
                payload
                    .get("category")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                payload.get("page").and_then(|v| v.as_u64()),
            ),
            "search-tools" => ("L1", Some("search".to_string()), None, None),
            "describe-tool" | "tool-info" => ("L2", Some("tool-detail".to_string()), None, None),
            "agent-manual" => {
                let section = payload
                    .get("section")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let section_lc = section
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .unwrap_or_default();
                let layer = match section_lc.as_str() {
                    "index" => "L0",
                    "tools" | "suggest" => "L1",
                    "tool-detail" => "L2",
                    _ => return None,
                };
                (
                    layer,
                    section,
                    payload
                        .get("category")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    payload.get("page").and_then(|v| v.as_u64()),
                )
            }
            _ => return None,
        };
        let token_estimate = (result.to_string().chars().count() / 4).max(1);
        Some(serde_json::json!({
            "layer": layer,
            "section": section,
            "category": category,
            "page": page,
            "tokens_returned": token_estimate,
        }))
    }

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

    #[allow(clippy::too_many_arguments)]
    fn finish_otel_tool_span(
        &self,
        tool_span: crate::otel_exporter::OtelSpan,
        task: &AgentTask,
        tool_name: &str,
        duration_ms: u64,
        success: bool,
        execution_mode: &str,
        error: Option<&str>,
    ) {
        tool_span.set_string_attribute("task.id", task.id.to_string());
        tool_span.set_string_attribute("agent.id", task.agent_id.to_string());
        tool_span.set_string_attribute("execution.mode", execution_mode);
        tool_span.set_bool_attribute("tool.success", success);
        tool_span.set_i64_attribute("tool.duration_ms", duration_ms as i64);
        if let Some(error) = error {
            tool_span.record_error(error);
        }
        self.otel
            .record_tool_metric(&task.agent_id.to_string(), tool_name, duration_ms, success);
    }

    fn record_otel_permission_denied(
        &self,
        parent: &crate::otel_exporter::OtelSpan,
        task: &AgentTask,
        tool_name: &str,
        deny_reason: &str,
    ) {
        let tool_span = self.otel.start_tool_span(parent, tool_name);
        tool_span.set_string_attribute("task.id", task.id.to_string());
        tool_span.set_string_attribute("agent.id", task.agent_id.to_string());
        tool_span.set_bool_attribute("tool.success", false);
        tool_span.add_event(
            "permission_check",
            vec![
                ("granted", "false".to_string()),
                ("deny_reason", deny_reason.to_string()),
            ],
        );
        tool_span.record_error(format!("Permission denied: {deny_reason}"));
        self.otel
            .record_tool_metric(&task.agent_id.to_string(), tool_name, 0, false);
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

        // Validate payload against registered JSON Schema (if any).
        //
        // Trust-tier behavior (see `SchemaRegistry::validate_for_dispatch`):
        // - Core/Verified manifests: fail-closed. A validation failure aborts
        //   dispatch with an `AgentOSError::ToolPayloadValidationFailed` whose
        //   message carries an RFC 6901 JSON Pointer to the offending field.
        // - Community/Blocked manifests: fail-open. The validator returns a
        //   soft diagnostic that is logged so operators can spot drift, but
        //   the tool's own Rust deserializer remains the authoritative gate
        //   for unaudited authors.
        match self
            .schema_registry
            .validate_for_dispatch(&tool_call.tool_name, &tool_call.payload)
        {
            Ok(Some(soft)) => {
                tracing::warn!(
                    task_id = %task.id,
                    tool = %tool_call.tool_name,
                    diag = %soft,
                    "Community-tier tool payload failed schema (soft, fail-open)"
                );
            }
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }

        let required_perms = self
            .tool_runner
            .get_required_permissions(&tool_call.tool_name)
            .unwrap_or_default();

        let required_for_validate: Vec<(String, PermissionOp)> = required_perms;

        self.capability_engine
            .validate_intent(&task.capability_token, &intent, &required_for_validate)
            .map_err(|e| format!("{}", e))
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_parallel_tool_calls(
        &self,
        task: &AgentTask,
        task_trace_id: &TraceID,
        iteration_span: &crate::otel_exporter::OtelSpan,
        iteration: u32,
        mut tool_calls: Vec<crate::tool_call::ParsedToolCall>,
        tool_call_count: &mut u32,
        refresh_knowledge_blocks: &mut bool,
        tool_not_found_suggest_count: &mut u32,
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
                    let err_str = e.to_string();
                    if err_str.contains("Task not found") {
                        tracing::warn!(error = %e, task_id = %task.id, "Task cancelled — skipping tool result push");
                    } else {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                    }
                    consecutive_push_failures += 1;
                    if consecutive_push_failures >= 3 {
                        anyhow::bail!(
                            "Task aborted: {} consecutive context push failures — agent context is unreliable",
                            consecutive_push_failures
                        );
                    }
                } else {
                    consecutive_push_failures = 0;
                }
            }
        }

        // Collect tool call IDs before the vector is consumed, so we can
        // increment reference counts after all results are pushed.
        let parallel_tool_call_ids: Vec<String> =
            tool_calls.iter().filter_map(|tc| tc.id.clone()).collect();

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

            // --- Connector routing: namespaced tool calls (e.g., "github.create_issue") ---
            // If the tool name contains a dot, try routing through the connector registry
            // before falling through to the normal tool registry lookup.
            if agentos_connectors::ConnectorRegistry::is_connector_call(&tool_call.tool_name) {
                // Permission check: require "connector.<id>:x" in the agent's PermissionSet.
                let connector_id = tool_call.tool_name.split('.').next().unwrap_or("");
                let connector_perm = format!("connector.{connector_id}");
                if !task
                    .capability_token
                    .permissions
                    .check(&connector_perm, PermissionOp::Execute)
                {
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id,
                        event_type: agentos_audit::AuditEventType::PermissionDenied,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "tool": tool_call.tool_name,
                            "required_permission": connector_perm,
                            "reason": "connector_permission_denied",
                        }),
                        severity: agentos_audit::AuditSeverity::Security,
                        reversible: false,
                        rollback_ref: None,
                    });
                    let error_result = serde_json::json!({
                        "error": format!("Permission denied: connector '{}' requires '{connector_perm}:x'", connector_id)
                    });
                    let _ = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &error_result,
                            tool_call.id.clone(),
                        )
                        .await;
                    continue;
                }

                if let Some(result) = self
                    .connector_registry
                    .route(&tool_call.tool_name, tool_call.payload.clone())
                    .await
                {
                    let (tool_result, is_error) = match &result {
                        Ok(value) => (value.clone(), false),
                        Err(e) => (serde_json::json!({ "error": e.to_string() }), true),
                    };

                    // Audit log every connector invocation
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id,
                        event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "tool": tool_call.tool_name,
                            "connector": connector_id,
                            "success": !is_error,
                        }),
                        severity: if is_error {
                            agentos_audit::AuditSeverity::Warn
                        } else {
                            agentos_audit::AuditSeverity::Info
                        },
                        reversible: false,
                        rollback_ref: None,
                    });

                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &tool_result,
                            tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, tool = %tool_call.tool_name, "Failed to push connector result to context");
                    }
                    continue;
                }
            }

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
                        let error_result = self
                            .build_tool_not_found_payload(
                                &tool_call.tool_name,
                                task.id,
                                task.agent_id,
                                trace_id,
                                tool_not_found_suggest_count,
                            )
                            .await;
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
                            let err_str = e.to_string();
                            if err_str.contains("Task not found") {
                                tracing::warn!(error = %e, task_id = %task.id, "Task cancelled — skipping tool result push");
                            } else {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            }
                            consecutive_push_failures += 1;
                            if consecutive_push_failures >= 3 {
                                anyhow::bail!(
                                    "Task aborted: {} consecutive context push failures — agent context is unreliable",
                                    consecutive_push_failures
                                );
                            }
                        } else {
                            consecutive_push_failures = 0;
                        }
                        self.trace_collector
                            .record_tool_call(
                                &task.id,
                                crate::trace_collector::TraceCollector::denied_tool_call(
                                    &tool_call.tool_name,
                                    tool_call.payload.clone(),
                                    "tool_not_registered",
                                ),
                            )
                            .await;
                        self.record_otel_permission_denied(
                            iteration_span,
                            task,
                            &tool_call.tool_name,
                            "tool_not_registered",
                        );
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
                        anyhow::bail!(
                            "Task aborted: {} consecutive context push failures — agent context is unreliable",
                            consecutive_push_failures
                        );
                    }
                } else {
                    consecutive_push_failures = 0;
                }
                self.trace_collector
                    .record_tool_call(
                        &task.id,
                        crate::trace_collector::TraceCollector::denied_tool_call(
                            &tool_call.tool_name,
                            tool_call.payload.clone(),
                            "tool_not_allowed_by_capability_token",
                        ),
                    )
                    .await;
                self.record_otel_permission_denied(
                    iteration_span,
                    task,
                    &tool_call.tool_name,
                    "tool_not_allowed_by_capability_token",
                );
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
                            anyhow::bail!(
                                "Task aborted: {} consecutive context push failures — agent context is unreliable",
                                consecutive_push_failures
                            );
                        }
                    } else {
                        consecutive_push_failures = 0;
                    }
                    self.trace_collector
                        .record_tool_call(
                            &task.id,
                            crate::trace_collector::TraceCollector::denied_tool_call(
                                &tool_call.tool_name,
                                tool_call.payload.clone(),
                                &denial_reason,
                            ),
                        )
                        .await;
                    self.record_otel_permission_denied(
                        iteration_span,
                        task,
                        &tool_call.tool_name,
                        &denial_reason,
                    );
                    continue;
                }
                Ok(IntentCoherenceResult::Rejected { reason }) => {
                    tracing::warn!(
                        task_id = %task.id,
                        tool = %tool_call.tool_name,
                        reason = %reason,
                        "Parallel tool-call coherence rejected"
                    );
                    let stop_directive = serde_json::json!({
                        "kernel_directive": "STOP",
                        "tool": tool_call.tool_name,
                        "reason": reason,
                        "instruction": "Do NOT call this tool again with similar arguments. This STOP applies to THIS TOOL only — the task is not over. Try a different tool, a different payload shape, or compose with sub-agents/memory/capabilities (see Task Feasibility & Persistence). Only if discovery via `search-tools` finds no alternative AND you can name the missing capability, summarise what you have and end."
                    });
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &stop_directive,
                            tool_call.id.clone(),
                        )
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                        consecutive_push_failures += 1;
                        if consecutive_push_failures >= 3 {
                            anyhow::bail!(
                                "Task aborted: {} consecutive context push failures — agent context is unreliable",
                                consecutive_push_failures
                            );
                        }
                    } else {
                        consecutive_push_failures = 0;
                    }
                    self.trace_collector
                        .record_tool_call(
                            &task.id,
                            crate::trace_collector::TraceCollector::denied_tool_call(
                                &tool_call.tool_name,
                                tool_call.payload.clone(),
                                &format!("coherence_rejected: {reason}"),
                            ),
                        )
                        .await;
                    self.record_otel_permission_denied(
                        iteration_span,
                        task,
                        &tool_call.tool_name,
                        &format!("coherence_rejected: {reason}"),
                    );
                    // Record the rejected call so the loop counter accumulates correctly
                    // across iterations. Without this, the counter resets to zero after each
                    // rejection and the agent can bypass the loop detector indefinitely.
                    self.intent_validator
                        .record_tool_call(&task.id, &tool_call)
                        .await;
                    let reject_count = self
                        .intent_validator
                        .increment_reject_count(&task.id, &tool_call.tool_name)
                        .await;
                    if reject_count >= crate::intent_validator::REJECT_FORCE_END_THRESHOLD {
                        tracing::warn!(
                            task_id = %task.id,
                            tool = %tool_call.tool_name,
                            reject_count,
                            "Forcing task EndTurn — model ignored prior STOP directive"
                        );
                        self.intent_validator.mark_force_end_turn(&task.id).await;
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
                            anyhow::bail!(
                                "Task aborted: {} consecutive context push failures — agent context is unreliable",
                                consecutive_push_failures
                            );
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
                        anyhow::bail!(
                            "Task aborted: {} consecutive context push failures — agent context is unreliable",
                            consecutive_push_failures
                        );
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
                        anyhow::bail!(
                            "Task aborted: {} consecutive context push failures — agent context is unreliable",
                            consecutive_push_failures
                        );
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
        let capability_snapshot = {
            let reg = self.capability_registry.read().await;
            CapabilityRegistrySnapshot::new(reg.list_capabilities())
        };
        let agent_snapshot_ref: Arc<dyn AgentRegistryQuery> = Arc::new(agent_snapshot);
        let task_snapshot_ref: Arc<dyn TaskQuery> = Arc::new(task_snapshot);
        let escalation_snapshot_ref: Arc<dyn EscalationQuery> = Arc::new(escalation_snapshot);
        let capability_snapshot_ref: Arc<dyn CapabilityRegistryQuery> =
            Arc::new(capability_snapshot);

        let fallback_timeout_secs = if task.autonomous {
            self.config.kernel.autonomous_mode.tool_timeout_seconds
        } else {
            self.config.kernel.tool_execution.default_timeout_seconds
        };
        let mut join_set = JoinSet::new();
        #[cfg(feature = "otel")]
        let otel_parent_context = iteration_span.parent_context();
        for call in prepared {
            let tool_runner = self.tool_runner.clone();
            let sandbox = self.sandbox.clone();
            let escalation_manager = Arc::clone(&self.escalation_manager);
            #[cfg(feature = "otel")]
            let otel = self.otel.clone();
            let data_dir = self.data_dir.clone();
            let ws_for_call = self.workspace_paths_for_agent(&task.agent_id);
            let workspace_paths = ws_for_call.read;
            let workspace_paths_writable = ws_for_call.writable;
            let workspace_paths_executable = ws_for_call.executable;
            let task_id = task.id;
            let agent_id = task.agent_id;
            let trace_id = call.trace_id;
            let permissions = task.capability_token.permissions.clone();
            let vault = self.vault.clone();
            let hal = self.hal.clone();
            let agent_registry = agent_snapshot_ref.clone();
            let task_registry = task_snapshot_ref.clone();
            let escalation_query = escalation_snapshot_ref.clone();
            let cap_registry = capability_snapshot_ref.clone();
            let cap_dispatcher: Arc<dyn CapabilityDispatcher> =
                Arc::clone(&self.capability_dispatcher) as Arc<dyn CapabilityDispatcher>;
            let zone_query: Arc<dyn StorageZoneQuery> = Arc::new(self.zone_table.clone());
            let order = call.order;
            let snapshot_ref = call.snapshot_ref;
            let tool_payload_preview = call.tool_payload_preview;
            let tool_call = call.tool_call;
            let sandbox_plan = call.sandbox_plan;
            let tool_cancellation = self.cancellation_token.child_token();
            let task_tool_categories = task.tool_categories.clone();
            let hook_registry = Arc::clone(&self.hook_registry);
            let execution_mode: &'static str = if sandbox_plan.is_some() {
                "sandbox"
            } else {
                "in_process"
            };
            #[cfg(feature = "otel")]
            let tool_parent_context = otel_parent_context.clone();

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
                    #[cfg(feature = "otel")]
                    let tool_span = otel
                        .start_tool_span_from_context(tool_parent_context, &tool_call.tool_name);
                    #[cfg(not(feature = "otel"))]
                    let tool_span = crate::otel_exporter::OtelSpan::default();
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
                        workspace_paths_writable,
                        workspace_paths_executable,
                        capability_registry: Some(cap_registry),
                        capability_dispatcher: Some(cap_dispatcher),
                        storage_zone_query: Some(zone_query),
                        cancellation_token: tool_cancellation,
                        tool_categories: task_tool_categories,
                    };

                    let tool_start = std::time::Instant::now();
                    let payload = tool_call.payload.clone();

                    // Fire ToolPre hook; abort if any hook denies the call.
                    let pre_result = hook_registry
                        .fire(&agentos_types::HookEvent::ToolPre {
                            task_id,
                            agent_id,
                            tool_name: tool_call.tool_name.clone(),
                            input_json: serde_json::to_string(&tool_call.payload)
                                .unwrap_or_default(),
                        })
                        .await;
                    if let agentos_types::HookResult::Abort(reason) = pre_result {
                        // ApprovalHook tags abort reasons with
                        // `approval_pending:<id>:` when it has installed
                        // a resolution channel for the escalation. In
                        // that case we PARK on the channel — on
                        // `Approved` we fall through to the execution
                        // block (without re-firing ToolPre); on `Denied`
                        // or expiry we surface a typed tool failure.
                        // Any other Abort is a hard hook denial.
                        let approval_pending_id = extract_approval_pending_id(&reason);
                        if let Some(esc_id) = approval_pending_id {
                            let outcome = wait_for_approval_resolution(
                                Arc::clone(&escalation_manager),
                                esc_id,
                            )
                            .await;
                            match outcome {
                                ApprovalWaitOutcome::Approved => {
                                    tracing::info!(
                                        task_id = %task_id,
                                        tool = %tool_call.tool_name,
                                        escalation_id = esc_id,
                                        "Approval resolved → resuming privileged tool call"
                                    );
                                    // Fall through to the execution block below.
                                }
                                ApprovalWaitOutcome::Denied => {
                                    return ParallelToolOutcome {
                                        order,
                                        tool_call: tool_call.clone(),
                                        trace_id,
                                        snapshot_ref,
                                        tool_payload_preview,
                                        duration_ms: tool_start.elapsed().as_millis() as u64,
                                        result: Err(
                                            agentos_types::AgentOSError::ToolExecutionFailed {
                                                tool_name: tool_call.tool_name.clone(),
                                                reason: format!(
                                                    "denied by user (escalation {esc_id})"
                                                ),
                                            },
                                        ),
                                        execution_mode,
                                    };
                                }
                                ApprovalWaitOutcome::Lost => {
                                    return ParallelToolOutcome {
                                        order,
                                        tool_call: tool_call.clone(),
                                        trace_id,
                                        snapshot_ref,
                                        tool_payload_preview,
                                        duration_ms: tool_start.elapsed().as_millis() as u64,
                                        result: Err(
                                            agentos_types::AgentOSError::ToolExecutionFailed {
                                                tool_name: tool_call.tool_name.clone(),
                                                reason: format!(
                                                    "approval channel for escalation {esc_id} \
                                                     was unavailable; the tool call did not run"
                                                ),
                                            },
                                        ),
                                        execution_mode,
                                    };
                                }
                            }
                        } else {
                            let tool_name = tool_call.tool_name.clone();
                            return ParallelToolOutcome {
                                order,
                                tool_call,
                                trace_id,
                                snapshot_ref,
                                tool_payload_preview,
                                duration_ms: tool_start.elapsed().as_millis() as u64,
                                result: Err(agentos_types::AgentOSError::ToolExecutionFailed {
                                    tool_name,
                                    reason: format!("Blocked by hook: {}", reason),
                                }),
                                execution_mode,
                            };
                        }
                    }

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

                    let duration_ms = tool_start.elapsed().as_millis() as u64;

                    // Fire ToolPost hook — informational, always fires regardless of result.
                    let output_json = match &result {
                        Ok(v) => serde_json::to_string(v).unwrap_or_default(),
                        Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
                    };
                    hook_registry
                        .fire(&agentos_types::HookEvent::ToolPost {
                            task_id,
                            agent_id,
                            tool_name: tool_call.tool_name.clone(),
                            output_json,
                            duration_ms,
                        })
                        .await;

                    tool_span.set_string_attribute("task.id", task_id.to_string());
                    tool_span.set_string_attribute("agent.id", agent_id.to_string());
                    tool_span.set_string_attribute("execution.mode", execution_mode);
                    tool_span.set_i64_attribute("tool.duration_ms", duration_ms as i64);
                    match &result {
                        Ok(_) => tool_span.set_bool_attribute("tool.success", true),
                        Err(err) => {
                            tool_span.set_bool_attribute("tool.success", false);
                            tool_span.record_error(err.to_string());
                        }
                    }

                    ParallelToolOutcome {
                        order,
                        tool_call,
                        trace_id,
                        snapshot_ref,
                        tool_payload_preview,
                        duration_ms,
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
                    self.otel.record_tool_metric(
                        &task.agent_id.to_string(),
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
                    self.tool_usage
                        .record(&task.agent_id.to_string(), &outcome.tool_call.tool_name)
                        .await;
                    // Update in-memory LRU for this agent (cap 10).
                    {
                        let tool_name = outcome.tool_call.tool_name.clone();
                        let mut lru = self.agent_tool_lru.write().await;
                        let entry = lru.entry(task.agent_id).or_default();
                        entry.retain(|n| n != &tool_name);
                        entry.push_front(tool_name);
                        if entry.len() > 10 {
                            entry.truncate(10);
                        }
                    }
                    if let Some(details) = Self::manual_query_details(
                        &outcome.tool_call.tool_name,
                        &outcome.tool_call.payload,
                        &result,
                    ) {
                        self.audit_log(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: outcome.trace_id,
                            event_type: agentos_audit::AuditEventType::ManualQuery,
                            agent_id: Some(task.agent_id),
                            task_id: Some(task.id),
                            tool_id: None,
                            details,
                            severity: agentos_audit::AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        });
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
                    // Phase 3 — Teaching envelope: wrap success results with
                    // manifest-derived `_meta` (use_for / prefer_over /
                    // related_tools) so the model learns the ecosystem from
                    // each call. Backward-compatible: tools without
                    // `usage_hints` declared get raw `tainted_result`.
                    let enriched_result = {
                        let registry = self.tool_registry.read().await;
                        let hints = registry
                            .get_by_name(&outcome.tool_call.tool_name)
                            .and_then(|t| t.manifest.usage_hints.as_ref())
                            .cloned();
                        drop(registry);
                        Self::wrap_with_manifest_meta(
                            tainted_result,
                            &outcome.tool_call.tool_name,
                            hints.as_ref(),
                        )
                    };
                    if let Err(e) = self
                        .context_manager
                        .push_tool_result(
                            &task.id,
                            &outcome.tool_call.tool_name,
                            &enriched_result,
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
                    self.trace_collector
                        .record_tool_call(
                            &task.id,
                            crate::trace_collector::TraceCollector::success_tool_call(
                                &outcome.tool_call.tool_name,
                                outcome.tool_call.payload.clone(),
                                context_result.clone(),
                                outcome.duration_ms,
                                outcome.snapshot_ref.clone(),
                                None,
                            ),
                        )
                        .await;
                }
                Err(e) => {
                    self.otel.record_tool_metric(
                        &task.agent_id.to_string(),
                        &outcome.tool_call.tool_name,
                        outcome.duration_ms,
                        false,
                    );
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
                    self.trace_collector
                        .record_tool_call(
                            &task.id,
                            crate::trace_collector::TraceCollector::failed_tool_call(
                                &outcome.tool_call.tool_name,
                                outcome.tool_call.payload.clone(),
                                &e.to_string(),
                                outcome.duration_ms,
                                outcome.snapshot_ref.clone(),
                            ),
                        )
                        .await;
                }
            }
        }

        // Increment reference counts for tool call IDs that were just processed.
        // This makes the linked Assistant + ToolResult entries resist eviction.
        if !parallel_tool_call_ids.is_empty() {
            if let Err(e) = self
                .context_manager
                .increment_references(&task.id, &parallel_tool_call_ids)
                .await
            {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "Failed to increment reference counts for parallel tool calls"
                );
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
        task_span: &crate::otel_exporter::OtelSpan,
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

        // Fire TaskStart hook — allows hooks to observe or cancel task startup.
        {
            let hook_event = agentos_types::HookEvent::TaskStart {
                task_id: task.id,
                agent_id: task.agent_id,
            };
            if let agentos_types::HookResult::Abort(reason) =
                self.hook_registry.fire(&hook_event).await
            {
                anyhow::bail!("Task aborted by hook: {}", reason);
            }
        }

        // `current_llm` is mutable so it can be swapped when a model downgrade is triggered.
        let mut current_llm = llm;
        task_span.set_string_attribute("llm.model", current_llm.model_name());
        // Track whether we've already downgraded this task to avoid repeated swaps.
        let mut model_downgraded = false;

        // Setup task context: system prompt, context window, user prompt, injection scan,
        // and adaptive retrieval plan. Returns Err if task should be aborted.
        // `retrieval_plan` is the seed plan classified from `task.original_prompt`;
        // it gets replaced in-loop when the conversation tail shifts (Phase 2).
        let (system_prompt, tools_desc, agent_directory, retrieval_plan) =
            self.setup_task_context(task, task_trace_id).await?;
        let mut current_retrieval_plan = retrieval_plan;
        // Seed last-query hash to the original prompt's hash so the first
        // iteration uses the seed plan without an unnecessary refresh trigger.
        let mut last_retrieval_query_hash: Option<u64> =
            Some(Self::hash_query(&task.original_prompt));

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
        let mut tool_not_found_suggest_count: u32 = 0;
        // Wall-clock start of the executor loop — used in the per-turn system
        // reminder so the model can see how long the task has been running.
        let task_start = std::time::Instant::now();

        // Cadence-gated context compactor. Reads tunables from
        // `[kernel.context_compaction]` so operators can adjust without a
        // recompile (Phase 5b). When `enable_llm_summarization = true`
        // the compactor calls the agent's current LLM adapter for a
        // semantic summary; on any LLM error it transparently falls back
        // to the extractive heuristic so a flaky model never breaks
        // task progress.
        let cc_cfg = &self.config.kernel.context_compaction;
        let context_compactor =
            ContextCompactor::new(cc_cfg.cadence, cc_cfg.keep_recent_iterations);

        for iteration in 0..max_iterations {
            completed_iterations = iteration + 1;

            // Best-effort compaction. Errors are logged and swallowed —
            // the iteration body must not fail because compaction stumbled.
            if completed_iterations > 0 {
                let llm_for_compaction = if cc_cfg.enable_llm_summarization {
                    Some(Arc::clone(&current_llm))
                } else {
                    None
                };
                match context_compactor
                    .maybe_compact_with_llm(
                        self,
                        &task.id,
                        completed_iterations as usize,
                        llm_for_compaction,
                    )
                    .await
                {
                    Ok(Some(outcome)) => {
                        tracing::info!(
                            task_id = %task.id,
                            compressed_entries = outcome.compressed_entries,
                            llm_summarized = outcome.llm_summarized,
                            "Context compacted into rolling summary"
                        );
                    }
                    Ok(None) => {} // not yet at cadence / not enough to compact
                    Err(e) => {
                        tracing::warn!(
                            task_id = %task.id,
                            error = %e,
                            "Context compaction failed; continuing without it"
                        );
                    }
                }
            }
            let iteration_trace_id = TraceID::new();
            let iteration_span = self.otel.start_iteration_span(
                task_span,
                completed_iterations,
                current_llm.model_name(),
            );
            iteration_span.set_string_attribute("task.id", task.id.to_string());
            iteration_span.set_string_attribute("agent.id", task.agent_id.to_string());

            // If the intent validator forced a final-synthesis pass on the
            // previous iteration, push a synthetic user nudge before the next
            // inference and run it with tools disabled. This bounds the
            // pathological case where a small model keeps emitting the same
            // rejected tool call regardless of `kernel_directive: STOP`.
            let final_synthesis_iteration =
                self.intent_validator.take_force_end_turn(&task.id).await;
            if final_synthesis_iteration {
                let nudge = agentos_types::ContextEntry::from_text(
                    agentos_types::ContextRole::User,
                    "[KERNEL] Tool calls have been disabled for this turn — the same tool was \
                     rejected repeatedly. Provide your final answer now using only the \
                     information already in your context. No tool calls.",
                );
                if let Err(e) = self.context_manager.push_entry(&task.id, nudge).await {
                    tracing::error!(
                        error = %e,
                        task_id = %task.id,
                        "Failed to push final-synthesis nudge — proceeding without it"
                    );
                }
            }

            let raw_context = match self.context_manager.get_context(&task.id).await {
                Ok(ctx) => ctx,
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Task not found") {
                        tracing::warn!(
                            error = %e,
                            task_id = %task.id,
                            iteration = iteration,
                            "Task cancelled — context no longer available"
                        );
                    } else {
                        tracing::error!(
                            error = %e,
                            task_id = %task.id,
                            iteration = iteration,
                            "Context manager failed to fetch context — aborting task"
                        );
                    }
                    anyhow::bail!(
                        "Task aborted at iteration {}: context manager error: {}",
                        iteration,
                        e
                    );
                }
            };

            // Phase 2 — Auto-RAG dynamic refresh.
            // Re-classify retrieval against the current conversation tail (last
            // user message + last tool result) so a topic pivot mid-task triggers
            // a fresh memory search instead of forever using `task.original_prompt`
            // as the query. The change-key (hash) intentionally excludes the
            // tool-result snippet — otherwise every successful tool call would
            // mutate the snippet and re-fire classify even on a single-topic
            // conversation. Only the latest user message decides "topic shifted".
            {
                let (dynamic_query, change_key) =
                    Self::build_dynamic_retrieval_query(&raw_context, &task.original_prompt);
                let change_hash = Self::hash_query(&change_key);
                if last_retrieval_query_hash != Some(change_hash) {
                    let dynamic_plan = self.retrieval_gate.classify(&dynamic_query);
                    if !dynamic_plan.is_empty() {
                        tracing::debug!(
                            task_id = %task.id,
                            iteration,
                            queries = dynamic_plan.queries.len(),
                            "Conversation tail shifted — refreshing retrieval plan"
                        );
                        current_retrieval_plan = dynamic_plan;
                        refresh_knowledge_blocks = true;
                    }
                    last_retrieval_query_hash = Some(change_hash);
                }
            }

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

                if !current_retrieval_plan.is_empty() && !is_event_triggered {
                    let outcome = self
                        .retrieval_executor
                        .execute(&current_retrieval_plan, Some(&task.agent_id))
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
                                "query_count": current_retrieval_plan.queries.len(),
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
                        retrieval_queries = current_retrieval_plan.queries.len(),
                        retrieval_results = retrieved.len(),
                        retrieval_blocks = knowledge_blocks.len(),
                        "Adaptive retrieval complete"
                    );
                } else if is_event_triggered && !current_retrieval_plan.is_empty() {
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

                // Agent context memory injection: per-agent self-curated document
                // injected at every task start, loaded once (Option A: next-invocation semantics).
                if self.config.memory.context.enabled {
                    match self
                        .context_memory_store
                        .read_content(&task.agent_id.to_string())
                        .await
                    {
                        Ok(Some(content)) => {
                            knowledge_blocks.insert(0, format!(
                                "<agent-context-memory>\nYour self-curated context memory. Update via context-memory-update tool. Write compressed: key:value pairs, short phrases, no prose. Every token here costs context budget.\n\n{}\n</agent-context-memory>",
                                content
                            ));
                            tracing::debug!(
                                task_id = %task.id,
                                "Injected agent context memory into knowledge blocks"
                            );
                        }
                        Ok(None) => {
                            // First task or empty memory: inject bootstrapping hint
                            knowledge_blocks.insert(0,
                                "<agent-context-memory>\nEmpty context memory. Use context-memory-update to save reusable knowledge for future tasks. Write compressed: key:value, short phrases, no prose. Budget is limited — every token counts.\n</agent-context-memory>".to_string()
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                task_id = %task.id,
                                error = %e,
                                "Failed to read agent context memory, skipping injection"
                            );
                        }
                    }
                }

                // Scratchpad context injection: search for related pages and inject
                // a BFS subgraph of linked notes as a knowledge block.
                if self.config.scratchpad.enabled {
                    let scratchpad_blocks = self
                        .inject_scratchpad_knowledge(
                            &task.agent_id,
                            &task.original_prompt,
                            &raw_context,
                        )
                        .await;
                    if !scratchpad_blocks.is_empty() {
                        knowledge_blocks.push(scratchpad_blocks);
                        tracing::debug!(
                            task_id = %task.id,
                            "Injected scratchpad context into knowledge blocks"
                        );
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

            // Build the per-turn reminder BEFORE `raw_context` is consumed by the
            // history filter below. The reminder cites the most recent tool
            // outcomes; `compiled_context` after the compactor may have evicted
            // them on long tasks, so we walk the unfiltered persistent context.
            let reminder_text = Self::build_turn_reminder(
                task,
                completed_iterations,
                tool_call_count,
                task_start.elapsed(),
                &raw_context,
            );

            // Filter history: only non-system Active entries
            let mut history: Vec<ContextEntry> = raw_context
                .entries
                .into_iter()
                .filter(|e| {
                    e.role != ContextRole::System && e.partition == ContextPartition::Active
                })
                .collect();

            // Scrub stale meta-tool results: keep only the latest ToolResult for
            // list-tools and search-tools so old paginated results don't stack up.
            scrub_meta_tool_results(&mut history);

            // Compile the optimized context window
            let mut compiled_context =
                self.context_compiler
                    .compile(crate::context_compiler::CompilationInputs {
                        system_prompt: system_prompt.clone(),
                        tool_descriptions: tools_desc.clone(),
                        agent_directory: agent_directory.clone(),
                        knowledge_blocks: knowledge_blocks.clone(),
                        history,
                        task_prompt: task.original_prompt.clone(),
                    });

            // Per-turn system reminder: re-inject world-state at the tail of the
            // prompt every iteration so the model never drifts from current
            // turn count, recent tool outcomes, elapsed time, and standing
            // rules. Built fresh each turn — never persisted to context_manager,
            // so the System-entry filter at the history step (above) does not
            // apply: this synthetic entry only lives inside the per-iteration
            // `compiled_context` and is rebuilt next turn from scratch.
            // Placed AFTER static prefix (system_prompt, tools, knowledge) so the
            // Anthropic prompt-cache prefix stays stable.
            compiled_context.entries.push(ContextEntry {
                role: ContextRole::System,
                parts: vec![ContentPart::Text {
                    text: reminder_text,
                }],
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.99,
                pinned: false,
                reference_count: 0,
                partition: ContextPartition::Active,
                category: ContextCategory::System,
                is_summary: false,
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

            // Build per-call options, wiring in thinking level from the task definition.
            // Prompt caching is always enabled — safe for non-Anthropic providers (they
            // ignore the flag) and provides up to 90% cost savings on repeated context.
            let inference_opts = agentos_llm::InferenceOptions {
                thinking_budget_tokens: task.thinking_level.budget_tokens(),
                enable_prompt_caching: true,
                ..Default::default()
            };

            let tools_for_inference: &[ToolManifest] = if final_synthesis_iteration {
                &[]
            } else {
                &llm_tool_manifests
            };
            // Watchdog-gated inference: rather than killing the call at the
            // soft threshold, we ask the user whether to keep waiting or
            // abort. The inference future stays alive across the gate so
            // no work is lost if the user picks Continue (or if the call
            // finishes naturally while we were asking). Scoped in its own
            // block so the borrow on `current_llm` ends before any later
            // model-downgrade reassignment in this iteration.
            let inference_outcome = {
                // Capture provider/model up front so log fields are
                // available without re-borrowing through `infer_fut`.
                let provider_name = current_llm.provider_name().to_string();
                let model_name = current_llm.model_name().to_string();
                let infer_fut = current_llm.infer_with_options(
                    &compiled_context,
                    tools_for_inference,
                    &inference_opts,
                );
                tokio::pin!(infer_fut);
                let inference_start = std::time::Instant::now();
                let mut extensions_used: u32 = 0;
                loop {
                    let threshold_sleep =
                        tokio::time::sleep(Duration::from_secs(LLM_INFERENCE_TIMEOUT_SECS));
                    tokio::pin!(threshold_sleep);

                    let step = tokio::select! {
                        biased;
                        res = &mut infer_fut => InferenceWatchdogStep::Completed(res),
                        _ = &mut threshold_sleep => InferenceWatchdogStep::Threshold,
                    };

                    match step {
                        InferenceWatchdogStep::Completed(res) => break res,
                        InferenceWatchdogStep::Threshold => {
                            let elapsed_secs = inference_start.elapsed().as_secs();
                            tracing::warn!(
                                task_id = %task.id,
                                agent_id = %task.agent_id,
                                provider = %provider_name,
                                model = %model_name,
                                elapsed_secs,
                                extensions_used,
                                "LLM inference exceeded soft threshold — escalating to user"
                            );

                            if extensions_used >= LLM_INFERENCE_MAX_EXTENSIONS {
                                tracing::error!(
                                    task_id = %task.id,
                                    agent_id = %task.agent_id,
                                    provider = %provider_name,
                                    model = %model_name,
                                    elapsed_secs,
                                    extensions_used,
                                    max = LLM_INFERENCE_MAX_EXTENSIONS,
                                    "LLM inference: max user-gate extensions reached, aborting"
                                );
                                self.context_manager.remove_context(&task.id).await;
                                self.intent_validator.remove_task(&task.id).await;
                                anyhow::bail!(
                                    "LLM inference timed out after {}s ({} extensions exhausted)",
                                    elapsed_secs,
                                    extensions_used
                                );
                            }

                            // Atomic create+install: closes the race in which a
                            // sink-driven user resolution arrives before we can
                            // park on the receiver. See
                            // `EscalationManager::create_escalation_with_resolution`.
                            let (esc_id, rx) = self
                                .escalation_manager
                                .create_escalation_with_resolution(
                                    task.id,
                                    task.agent_id,
                                    crate::kernel_action::EscalationReason::Other(
                                        "long_running_inference".to_string(),
                                    ),
                                    format!(
                                        "Agent has been running an LLM inference for {}s (extension {}/{}).",
                                        elapsed_secs,
                                        extensions_used + 1,
                                        LLM_INFERENCE_MAX_EXTENSIONS
                                    ),
                                    "Agent is taking longer than expected. Continue waiting, or abort?"
                                        .to_string(),
                                    vec!["Continue".to_string(), "Abort".to_string()],
                                    "high".to_string(),
                                    true,
                                    TraceID::new(),
                                    Some(crate::escalation::AutoAction::Deny),
                                )
                                .await;

                            if esc_id == u64::MAX {
                                tracing::error!(
                                    task_id = %task.id,
                                    agent_id = %task.agent_id,
                                    provider = %provider_name,
                                    model = %model_name,
                                    "LLM inference: escalation cap reached, aborting"
                                );
                                self.context_manager.remove_context(&task.id).await;
                                self.intent_validator.remove_task(&task.id).await;
                                anyhow::bail!(
                                    "LLM inference timed out after {}s — escalation cap reached",
                                    elapsed_secs
                                );
                            }

                            if rx.is_none() {
                                tracing::warn!(
                                    task_id = %task.id,
                                    escalation_id = esc_id,
                                    "LLM inference: resolution channel unavailable; falling back to grace-only abort"
                                );
                            }

                            let grace = tokio::time::sleep(Duration::from_secs(
                                LLM_INFERENCE_USER_GRACE_SECS,
                            ));
                            tokio::pin!(grace);

                            let gate_step = match rx {
                                Some(rx) => tokio::select! {
                                    biased;
                                    res = &mut infer_fut => InferenceGateStep::Completed(res),
                                    outcome = rx => match outcome {
                                        Ok(crate::escalation::ResolutionOutcome::Approved) => {
                                            InferenceGateStep::Continue
                                        }
                                        Ok(crate::escalation::ResolutionOutcome::Denied) => {
                                            InferenceGateStep::Abort
                                        }
                                        Err(_) => InferenceGateStep::Abort,
                                    },
                                    _ = &mut grace => InferenceGateStep::Abort,
                                },
                                None => tokio::select! {
                                    biased;
                                    res = &mut infer_fut => InferenceGateStep::Completed(res),
                                    _ = &mut grace => InferenceGateStep::Abort,
                                },
                            };

                            match gate_step {
                                InferenceGateStep::Completed(res) => {
                                    // Inference finished while the user-gate
                                    // was open — auto-resolve as approved so
                                    // no escalation is left orphaned.
                                    let _ = self
                                        .escalation_manager
                                        .resolve(esc_id, "approved".to_string())
                                        .await;
                                    break res;
                                }
                                InferenceGateStep::Continue => {
                                    // Close out this escalation cleanly so it
                                    // does not linger and consume a slot from
                                    // `MAX_ESCALATIONS_PER_TASK` for 5 minutes
                                    // (until the auto-deny sweeper expires it).
                                    let _ = self
                                        .escalation_manager
                                        .resolve(esc_id, "approved".to_string())
                                        .await;
                                    extensions_used += 1;
                                    tracing::info!(
                                        task_id = %task.id,
                                        agent_id = %task.agent_id,
                                        provider = %provider_name,
                                        model = %model_name,
                                        escalation_id = esc_id,
                                        extensions_used,
                                        "User approved continuation — re-arming watchdog"
                                    );
                                    continue;
                                }
                                InferenceGateStep::Abort => {
                                    let _ = self
                                        .escalation_manager
                                        .resolve(esc_id, "denied".to_string())
                                        .await;
                                    let total_secs = inference_start.elapsed().as_secs();
                                    tracing::error!(
                                        task_id = %task.id,
                                        agent_id = %task.agent_id,
                                        provider = %provider_name,
                                        model = %model_name,
                                        escalation_id = esc_id,
                                        total_secs,
                                        "LLM inference aborted at user gate"
                                    );
                                    self.context_manager.remove_context(&task.id).await;
                                    self.intent_validator.remove_task(&task.id).await;
                                    anyhow::bail!(
                                        "LLM inference aborted after {}s (user denied or no response)",
                                        total_secs
                                    );
                                }
                            }
                        }
                    }
                }
            };

            let inference = match inference_outcome {
                Err(e) => {
                    self.context_manager.remove_context(&task.id).await;
                    self.intent_validator.remove_task(&task.id).await;
                    anyhow::bail!("LLM error: {}", e);
                }
                Ok(mut result) => {
                    if result.uncertainty.is_none() {
                        result.uncertainty = agentos_llm::parse_uncertainty(&result.text);
                    }
                    result
                }
            };

            // Forced final-synthesis pass: take whatever text the model
            // produced (tools were already disabled above) and end the task.
            // Skip every tool-call extraction path so a stray ```json fence
            // can no longer re-enter the loop.
            if final_synthesis_iteration {
                tracing::info!(
                    task_id = %task.id,
                    iteration = completed_iterations,
                    text_len = inference.text.len(),
                    "Final synthesis pass produced text — ending task"
                );
                final_answer = if inference.text.trim().is_empty() {
                    "[KERNEL] Task ended after repeated tool-call rejections; no final answer was produced."
                        .to_string()
                } else {
                    inference.text
                };
                break;
            }

            crate::metrics::record_inference(
                current_llm.provider_name(),
                current_llm.model_name(),
                inference.tokens_used.prompt_tokens,
                inference.tokens_used.completion_tokens,
                inference.duration_ms,
            );
            self.otel.record_llm_request(
                &task.agent_id.to_string(),
                current_llm.provider_name(),
                current_llm.model_name(),
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
            self.trace_collector
                .begin_iteration(
                    &task.id,
                    iteration,
                    current_llm.model_name(),
                    inference.tokens_used.prompt_tokens,
                    inference.tokens_used.completion_tokens,
                    &format!("{:?}", inference.stop_reason),
                    None,
                )
                .await;
            iteration_span.set_i64_attribute(
                "llm.input_tokens",
                inference.tokens_used.prompt_tokens as i64,
            );
            iteration_span.set_i64_attribute(
                "llm.output_tokens",
                inference.tokens_used.completion_tokens as i64,
            );
            iteration_span.set_i64_attribute("llm.duration_ms", inference.duration_ms as i64);
            iteration_span
                .set_string_attribute("llm.stop_reason", format!("{:?}", inference.stop_reason));
            let iteration_cost_usd = inference
                .cost
                .as_ref()
                .map(|cost| cost.total_cost_usd)
                .unwrap_or(0.0);
            self.otel.record_cost(
                &task.agent_id.to_string(),
                current_llm.model_name(),
                iteration_cost_usd,
                inference.tokens_used.prompt_tokens,
                inference.tokens_used.completion_tokens,
            );
            iteration_span.set_f64_attribute("llm.cost_usd", iteration_cost_usd);
            if let Some(cost_snap) = self.cost_tracker.get_snapshot(&task.agent_id).await {
                self.trace_collector
                    .update_cost(&task.id, cost_snap.cost_usd)
                    .await;
                iteration_span.set_f64_attribute("task.cost_usd", cost_snap.cost_usd);
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
                                task.id,
                                provider,
                                downgrade_to
                            );
                            current_llm = cheaper_llm;
                            model_downgraded = true;
                        } else {
                            tracing::warn!(
                                "Task {} downgrade model {}/{} not available — falling through to PauseRequired",
                                task.id,
                                provider,
                                downgrade_to
                            );
                            self.context_manager.remove_context(&task.id).await;
                            self.intent_validator.remove_task(&task.id).await;
                            anyhow::bail!(
                                "Budget pause threshold reached: {} at {:.1}% (downgrade model unavailable)",
                                resource,
                                current_pct
                            );
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
                        parts: vec![ContentPart::Text {
                            text: inference.text.clone(),
                        }],
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
                } else {
                    // Multi-block fallback: small models often emit several
                    // ```json {tool:..., payload:...} ``` fences inside the
                    // response text. Recover them so the kernel can execute.
                    let recovered = crate::tool_call::extract_text_tool_calls(&inference.text);
                    if !recovered.is_empty() {
                        let names: Vec<String> =
                            recovered.iter().map(|c| c.tool_name.clone()).collect();
                        tracing::info!(
                            task_id = %task.id,
                            count = recovered.len(),
                            tools = ?names,
                            "Recovered tool calls from JSON-in-markdown text content"
                        );
                        self.audit_log(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: *task_trace_id,
                            event_type: agentos_audit::AuditEventType::ToolCallRecovered,
                            agent_id: Some(task.agent_id),
                            task_id: Some(task.id),
                            tool_id: None,
                            details: serde_json::json!({
                                "count": recovered.len(),
                                "tools": names,
                            }),
                            severity: agentos_audit::AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        });
                        parsed_tool_calls = recovered;
                    }
                }
            }
            if parsed_tool_calls.len() > 1 {
                if self.config.kernel.tool_calls.allow_parallel {
                    self.execute_parallel_tool_calls(
                        task,
                        task_trace_id,
                        &iteration_span,
                        iteration,
                        parsed_tool_calls,
                        &mut tool_call_count,
                        &mut refresh_knowledge_blocks,
                        &mut tool_not_found_suggest_count,
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
                                // Record the rejected call so the loop counter accumulates across
                                // iterations and the agent cannot bypass the detector indefinitely.
                                self.intent_validator
                                    .record_tool_call(&task.id, &tool_call)
                                    .await;
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

                                let error_result = self
                                    .build_tool_not_found_payload(
                                        &tool_call.tool_name,
                                        task.id,
                                        task.agent_id,
                                        trace_id,
                                        &mut tool_not_found_suggest_count,
                                    )
                                    .await;
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
                                self.trace_collector
                                    .record_tool_call(
                                        &task.id,
                                        crate::trace_collector::TraceCollector::denied_tool_call(
                                            &tool_call.tool_name,
                                            tool_call.payload.clone(),
                                            "tool_not_registered",
                                        ),
                                    )
                                    .await;
                                self.record_otel_permission_denied(
                                    &iteration_span,
                                    task,
                                    &tool_call.tool_name,
                                    "tool_not_registered",
                                );
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
                                self.trace_collector
                                    .record_tool_call(
                                        &task.id,
                                        crate::trace_collector::TraceCollector::denied_tool_call(
                                            &tool_call.tool_name,
                                            tool_call.payload.clone(),
                                            "tool_not_allowed_by_capability_token",
                                        ),
                                    )
                                    .await;
                                self.record_otel_permission_denied(
                                    &iteration_span,
                                    task,
                                    &tool_call.tool_name,
                                    "tool_not_allowed_by_capability_token",
                                );
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
                            self.trace_collector
                                .record_tool_call(
                                    &task.id,
                                    crate::trace_collector::TraceCollector::denied_tool_call(
                                        &tool_call.tool_name,
                                        tool_call.payload.clone(),
                                        &denial_reason,
                                    ),
                                )
                                .await;
                            self.record_otel_permission_denied(
                                &iteration_span,
                                task,
                                &tool_call.tool_name,
                                &denial_reason,
                            );
                            continue;
                        }
                        Ok(IntentCoherenceResult::Rejected { reason }) => {
                            tracing::warn!(
                                "Task {} coherence rejected for tool {}: {}",
                                task.id,
                                tool_call.tool_name,
                                reason
                            );
                            let stop_directive = serde_json::json!({
                                "kernel_directive": "STOP",
                                "tool": tool_call.tool_name,
                                "reason": reason,
                                "instruction": "Do NOT call this tool again with similar arguments. This STOP applies to THIS TOOL only — the task is not over. Try a different tool, a different payload shape, or compose with sub-agents/memory/capabilities (see Task Feasibility & Persistence). Only if discovery via `search-tools` finds no alternative AND you can name the missing capability, summarise what you have and end."
                            });
                            if let Err(e) = self
                                .context_manager
                                .push_tool_result(
                                    &task.id,
                                    &tool_call.tool_name,
                                    &stop_directive,
                                    tool_call.id.clone(),
                                )
                                .await
                            {
                                tracing::error!(error = %e, task_id = %task.id, "Failed to push tool result to context — agent may not see this result on next iteration");
                            }
                            self.trace_collector
                                .record_tool_call(
                                    &task.id,
                                    crate::trace_collector::TraceCollector::denied_tool_call(
                                        &tool_call.tool_name,
                                        tool_call.payload.clone(),
                                        &format!("coherence_rejected: {reason}"),
                                    ),
                                )
                                .await;
                            self.record_otel_permission_denied(
                                &iteration_span,
                                task,
                                &tool_call.tool_name,
                                &format!("coherence_rejected: {reason}"),
                            );
                            // Record the rejected call so the loop counter accumulates across
                            // iterations and the agent cannot bypass the detector indefinitely.
                            self.intent_validator
                                .record_tool_call(&task.id, &tool_call)
                                .await;
                            let reject_count = self
                                .intent_validator
                                .increment_reject_count(&task.id, &tool_call.tool_name)
                                .await;
                            if reject_count >= crate::intent_validator::REJECT_FORCE_END_THRESHOLD {
                                tracing::warn!(
                                    task_id = %task.id,
                                    tool = %tool_call.tool_name,
                                    reject_count,
                                    "Forcing task EndTurn — model ignored prior STOP directive"
                                );
                                self.intent_validator.mark_force_end_turn(&task.id).await;
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
                                    "risk_level": "SoftApproval",
                                }),
                                severity: agentos_audit::AuditSeverity::Warn,
                                reversible: false,
                                rollback_ref: None,
                            });
                            // Create a non-blocking soft-approval (30s review window).
                            self.escalation_manager
                                .create_soft_approval(
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
                                    trace_id,
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

                    let ws_sync = self.workspace_paths_for_agent(&task.agent_id);
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
                        workspace_paths: ws_sync.read,
                        workspace_paths_writable: ws_sync.writable,
                        workspace_paths_executable: ws_sync.executable,
                        capability_registry: {
                            let reg = self.capability_registry.read().await;
                            Some(
                                Arc::new(CapabilityRegistrySnapshot::new(reg.list_capabilities()))
                                    as Arc<dyn CapabilityRegistryQuery>,
                            )
                        },
                        capability_dispatcher: Some(Arc::clone(&self.capability_dispatcher)
                            as Arc<dyn CapabilityDispatcher>),
                        storage_zone_query: Some(
                            Arc::new(self.zone_table.clone()) as Arc<dyn StorageZoneQuery>
                        ),
                        cancellation_token: self.cancellation_token.child_token(),
                        tool_categories: task.tool_categories.clone(),
                    };
                    let tool_payload_preview = Self::truncate_for_prompt_payload(
                        &serde_json::to_string(&tool_call.payload).unwrap_or_default(),
                        600,
                    );

                    let tool_start = std::time::Instant::now();
                    let tool_span = self
                        .otel
                        .start_tool_span(&iteration_span, &tool_call.tool_name);
                    // Capture input payload for trace before it is moved into the executor.
                    let seq_input_json = tool_call.payload.clone();
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
                            let seq_duration_ms = tool_start.elapsed().as_millis() as u64;
                            let memory_mutating_tool = matches!(
                                tool_call.tool_name.as_str(),
                                "memory-write" | "archival-insert"
                            );
                            crate::metrics::record_tool_execution(
                                &tool_call.tool_name,
                                seq_duration_ms,
                                true,
                            );
                            self.finish_otel_tool_span(
                                tool_span,
                                task,
                                &tool_call.tool_name,
                                seq_duration_ms,
                                true,
                                execution_mode,
                                None,
                            );
                            self.trace_collector
                                .record_tool_call(
                                    &task.id,
                                    crate::trace_collector::TraceCollector::success_tool_call(
                                        &tool_call.tool_name,
                                        seq_input_json.clone(),
                                        result.clone(),
                                        seq_duration_ms,
                                        snapshot_ref.clone(),
                                        None,
                                    ),
                                )
                                .await;
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
                                        "duration_ms": seq_duration_ms,
                                        "execution_mode": execution_mode,
                                    }),
                                    chain_depth,
                                    Some(trace_id),
                                    Some(task.agent_id),
                                    Some(task.id),
                                )
                                .await;
                            }
                            self.tool_usage
                                .record(&task.agent_id.to_string(), &tool_call.tool_name)
                                .await;
                            // Update in-memory LRU for this agent (cap 10).
                            {
                                let tool_name = tool_call.tool_name.clone();
                                let mut lru = self.agent_tool_lru.write().await;
                                let entry = lru.entry(task.agent_id).or_default();
                                entry.retain(|n| n != &tool_name);
                                entry.push_front(tool_name);
                                if entry.len() > 10 {
                                    entry.truncate(10);
                                }
                            }
                            if let Some(details) = Self::manual_query_details(
                                &tool_call.tool_name,
                                &seq_input_json,
                                &result,
                            ) {
                                self.audit_log(agentos_audit::AuditEntry {
                                    timestamp: chrono::Utc::now(),
                                    trace_id,
                                    event_type: agentos_audit::AuditEventType::ManualQuery,
                                    agent_id: Some(task.agent_id),
                                    task_id: Some(task.id),
                                    tool_id: None,
                                    details,
                                    severity: agentos_audit::AuditSeverity::Info,
                                    reversible: false,
                                    rollback_ref: None,
                                });
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
                                    task.id,
                                    tool_call.tool_name,
                                    pattern_names,
                                    threat
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
                                        anyhow::bail!(
                                            "Task aborted: {} consecutive context push failures — agent context is unreliable",
                                            consecutive_push_failures
                                        );
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
                            let seq_fail_duration_ms = tool_start.elapsed().as_millis() as u64;
                            self.finish_otel_tool_span(
                                tool_span,
                                task,
                                &tool_call.tool_name,
                                seq_fail_duration_ms,
                                false,
                                execution_mode,
                                Some(&e.to_string()),
                            );
                            self.trace_collector
                                .record_tool_call(
                                    &task.id,
                                    crate::trace_collector::TraceCollector::failed_tool_call(
                                        &tool_call.tool_name,
                                        seq_input_json,
                                        &e.to_string(),
                                        seq_fail_duration_ms,
                                        snapshot_ref.clone(),
                                    ),
                                )
                                .await;
                            crate::metrics::record_tool_execution(
                                &tool_call.tool_name,
                                seq_fail_duration_ms,
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

                    // Increment reference counts for the tool call ID that was just processed.
                    // This makes the linked Assistant + ToolResult entries resist eviction.
                    if let Some(ref tc_id) = tool_call.id {
                        if let Err(e) = self
                            .context_manager
                            .increment_references(&task.id, std::slice::from_ref(tc_id))
                            .await
                        {
                            tracing::warn!(
                                task_id = %task.id,
                                error = %e,
                                "Failed to increment reference counts for tool call"
                            );
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
                                    parts: vec![agentos_types::ContentPart::Text {
                                        text: reprompt.to_string(),
                                    }],
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

            // Write a checkpoint at the end of each iteration (after all tool calls).
            // Skipped for ephemeral tasks or when tool_call_count is 0 (no state to save).
            if !task.skip_checkpoint && tool_call_count > 0 {
                if let Ok(context) = self.context_manager.get_context(&task.id).await {
                    let persisted_ctx = crate::context::PersistedTaskContext {
                        window: context,
                        agent_id: task.agent_id,
                        injected_sub_agents: Vec::new(),
                    };
                    let payload = crate::checkpoint_store::CheckpointPayload {
                        schema_version: crate::checkpoint_store::CHECKPOINT_SCHEMA_VERSION,
                        key_version: crate::checkpoint_store::CHECKPOINT_KEY_VERSION,
                        task: task.clone(),
                        context: persisted_ctx,
                        tool_call_history: Vec::new(),
                    };
                    match serde_json::to_vec(&payload) {
                        Ok(state_blob) => {
                            let record = crate::checkpoint_store::CheckpointRecord {
                                checkpoint_id: uuid::Uuid::new_v4().to_string(),
                                task_id: task.id,
                                agent_id: task.agent_id,
                                step_num: completed_iterations,
                                created_at: chrono::Utc::now(),
                                updated_at: chrono::Utc::now(),
                                schema_version: crate::checkpoint_store::CHECKPOINT_SCHEMA_VERSION,
                                key_version: crate::checkpoint_store::CHECKPOINT_KEY_VERSION,
                                state_blob,
                            };
                            if let Err(e) = self.checkpoint_store.write(record).await {
                                tracing::warn!(
                                    task_id = %task.id,
                                    iteration = completed_iterations,
                                    error = %e,
                                    "Checkpoint write failed — task continues without checkpoint"
                                );
                            } else {
                                self.hook_registry
                                    .fire(&agentos_types::HookEvent::CheckpointWritten {
                                        task_id: task.id,
                                    })
                                    .await;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                task_id = %task.id,
                                error = %e,
                                "Failed to serialize checkpoint payload"
                            );
                        }
                    }
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

        // Clean up checkpoint on normal completion (no resume needed).
        if let Err(e) = self.checkpoint_store.delete_for_task(&task.id).await {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "Failed to delete checkpoint after task completion"
            );
        }

        // Fire TaskEnd hook (informational — result already computed).
        self.hook_registry
            .fire(&agentos_types::HookEvent::TaskEnd {
                task_id: task.id,
                agent_id: task.agent_id,
                success: true,
            })
            .await;
        self.context_manager.remove_context(&task.id).await;
        self.intent_validator.remove_task(&task.id).await;

        Ok(TaskResult {
            answer: final_answer,
            tool_call_count,
            iterations: completed_iterations,
            // FOLLOWUP: thread per-record `ToolCallRecord` through the
            // executor so scheduled run history shows tool-by-tool detail.
            // Currently only the count is recorded. When wiring, populate
            // `ToolCallRecord.tool_call_id` from each `InferenceToolCall.id`
            // — native Anthropic/OpenAI calls carry the provider tool_use_id,
            // and checkpoint replay needs it to reconstruct the assistant
            // turn's tool_calls array on resume.
            tool_calls: Vec::new(),
        })
    }

    /// Execute a task from the background executor loop.
    #[tracing::instrument(skip_all, fields(task_id = %task.id, agent_id = %task.agent_id))]
    pub(crate) async fn execute_task(&self, task: &AgentTask) {
        let start = std::time::Instant::now();
        let task_trace_id = TraceID::new();
        let task_span =
            self.otel
                .start_task_span(&task.id.to_string(), &task.agent_id.to_string(), "");
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
        self.trace_collector
            .start_task(task.id, task.agent_id, &task.original_prompt)
            .await;
        self.otel.adjust_active_tasks(1);

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

        match self
            .execute_task_sync(task, &task_trace_id, &task_span)
            .await
        {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                self.trace_collector
                    .finish_task(&task.id, "Complete", chrono::Utc::now())
                    .await;
                task_span.set_string_attribute("task.status", "complete");
                task_span.set_i64_attribute("task.iterations", result.iterations as i64);
                self.otel
                    .record_task_metric(&task.agent_id.to_string(), "complete", duration_ms);
                self.complete_task_success(task, &result, duration_ms, task_trace_id)
                    .await;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                // Fire TaskEnd with success=false so hooks observing task lifecycle
                // always see a symmetric start/end pair regardless of outcome.
                self.hook_registry
                    .fire(&agentos_types::HookEvent::TaskEnd {
                        task_id: task.id,
                        agent_id: task.agent_id,
                        success: false,
                    })
                    .await;
                self.trace_collector
                    .finish_task(&task.id, "Failed", chrono::Utc::now())
                    .await;
                task_span.set_string_attribute("task.status", "failed");
                task_span.record_error(e.to_string());
                self.otel
                    .record_task_metric(&task.agent_id.to_string(), "failed", duration_ms);
                self.complete_task_failure(task, e, duration_ms, task_trace_id)
                    .await;
            }
        }
        self.otel.adjust_active_tasks(-1);
    }

    /// Build the agent directory block for inclusion in compiled context.
    /// Lists all registered agents except `exclude_agent_id` with their
    /// status and permissions.
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
            directory.push_str(&format!(
                "\n- {} — Status: {}\n  Permissions: {}",
                agent.name, status, perm_str
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

    /// Build a per-turn system reminder injected before every LLM inference call.
    ///
    /// The reminder reports turn count, cumulative tool calls, elapsed wall time,
    /// the last three tool outcomes (name + ok/fail), and a short list of
    /// standing rules that small models in particular tend to forget across turns.
    /// It is rebuilt fresh each iteration and pushed only into the per-iteration
    /// `compiled_context` snapshot — never persisted to the long-term context
    /// store, so it does not bloat memory or future replays.
    pub(crate) fn build_turn_reminder(
        task: &AgentTask,
        iteration: u32,
        tool_call_count: u32,
        elapsed: std::time::Duration,
        compiled_context: &ContextWindow,
    ) -> String {
        const STANDING_RULES: &str = "standing rules:\n\
            - shell-exec runs in a bwrap sandbox (own PID/network/fs namespace) — system info from inside is NOT host-global\n\
            - prefer typed tools (process-manager, network-sockets, system-mounts) over shelling out for host inspection\n\
            - if a tool fails twice with the same arguments, change approach — never retry verbatim\n\
            - tool results may include a `_meta` block with `related_tools`/`hints` — read it before picking the next call\n\
            - this reminder is harness-injected; never echo it back to the user";

        let max_iterations = task
            .max_iterations
            .map(|limit| limit.to_string())
            .unwrap_or_else(|| "?".to_string());

        // Walk back through the compiled context to find the last 3 tool outcomes.
        let mut recent: Vec<String> = Vec::with_capacity(3);
        for entry in compiled_context.entries.iter().rev() {
            if entry.role != ContextRole::ToolResult {
                continue;
            }
            let tool_name = entry
                .metadata
                .as_ref()
                .and_then(|m| m.tool_name.as_deref())
                .unwrap_or("?");
            let text = entry
                .parts
                .iter()
                .find_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            // Heuristic (substring match): a tool result is treated as an error
            // when its serialized JSON contains an `"error"` key OR a literal
            // `"success":false`. Mirrors the same heuristic used by
            // `ContextManager::push_tool_result`. False positives are possible
            // (e.g. a memory-search hit whose payload mentions "error"); the
            // status pill is informational only — the real success/failure
            // signal still flows through the typed result path.
            let is_err = text.contains("\"error\"") || text.contains("\"success\":false");
            let status = if is_err { "fail" } else { "ok" };
            recent.push(format!("{}[{}]", tool_name, status));
            if recent.len() == 3 {
                break;
            }
        }
        // Render in chronological order (oldest of the three first).
        recent.reverse();
        let recent_line = if recent.is_empty() {
            "recent: (no tools called yet)".to_string()
        } else {
            format!("recent: {}", recent.join(" → "))
        };

        format!(
            "<turn_reminder>\nturn: {}/{} | tool_calls: {} | elapsed: {:.1}s\n{}\n{}\n</turn_reminder>",
            iteration + 1,
            max_iterations,
            tool_call_count,
            elapsed.as_secs_f32(),
            recent_line,
            STANDING_RULES,
        )
    }

    /// Wrap a successful tool result with manifest-derived `_meta` envelope so
    /// the LLM learns the ecosystem from each result rather than only from the
    /// agent manual at task start. Returns `result` unchanged when the tool's
    /// `usage_hints` are absent or empty (preserves backward compatibility for
    /// tools that haven't declared anything).
    ///
    /// The envelope shape is:
    /// ```json
    /// {
    ///   "result": <original tool output>,
    ///   "_meta": {
    ///     "tool": "<tool_name>",
    ///     "use_for": [...],
    ///     "prefer_over": [...],
    ///     "related_tools": [...]
    ///   }
    /// }
    /// ```
    /// Tool-side parsers ignore unknown keys, so wrapping is safe for downstream
    /// consumers that only read fields under `result`.
    pub(crate) fn wrap_with_manifest_meta(
        result: serde_json::Value,
        tool_name: &str,
        hints: Option<&agentos_types::UsageHints>,
    ) -> serde_json::Value {
        let Some(h) = hints else {
            return result;
        };
        if h.use_for.is_empty() && h.prefer_over.is_empty() && h.related_tools.is_empty() {
            return result;
        }
        let mut meta = serde_json::Map::new();
        meta.insert(
            "tool".to_string(),
            serde_json::Value::String(tool_name.to_string()),
        );
        if !h.use_for.is_empty() {
            meta.insert("use_for".to_string(), serde_json::json!(h.use_for));
        }
        if !h.prefer_over.is_empty() {
            meta.insert("prefer_over".to_string(), serde_json::json!(h.prefer_over));
        }
        if !h.related_tools.is_empty() {
            meta.insert(
                "related_tools".to_string(),
                serde_json::json!(h.related_tools),
            );
        }
        serde_json::json!({
            "result": result,
            "_meta": serde_json::Value::Object(meta),
        })
    }

    /// Build a dynamic retrieval query from the most recent conversation tail.
    ///
    /// The retrieval plan is otherwise classified ONCE from `task.original_prompt`
    /// at task setup, so when the conversation pivots ("now look at the database
    /// side") the original keyword set keeps driving every memory search. This
    /// helper composes a fresh query each iteration from the latest user message
    /// and the latest tool result so the classifier can pick a new plan when the
    /// topic actually shifts.
    ///
    /// Returns a `(query, change_key)` tuple:
    /// - `query` — full string passed to `RetrievalGate::classify` (user + tool snippet)
    /// - `change_key` — stable signal used for hash-based change detection. ONLY
    ///   includes the latest user message (or fallback). The tool snippet is
    ///   intentionally excluded from the change key so a routine tool call that
    ///   doesn't shift the topic does NOT thrash the classifier every iteration.
    ///   When `change_key` matches the previous turn's hash, retrieval skips.
    pub(crate) fn build_dynamic_retrieval_query(
        raw_context: &ContextWindow,
        fallback: &str,
    ) -> (String, String) {
        const USER_BUDGET: usize = 500;
        const TOOL_BUDGET: usize = 200;

        let mut latest_user: Option<String> = None;
        let mut latest_tool: Option<(String, String)> = None;

        for entry in raw_context.entries.iter().rev() {
            let text = entry
                .parts
                .iter()
                .find_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            match entry.role {
                ContextRole::User if latest_user.is_none() => {
                    latest_user = Some(Self::truncate_chars(text, USER_BUDGET));
                }
                ContextRole::ToolResult if latest_tool.is_none() => {
                    let tool_name = entry
                        .metadata
                        .as_ref()
                        .and_then(|m| m.tool_name.as_deref())
                        .unwrap_or("?")
                        .to_string();
                    latest_tool = Some((tool_name, Self::truncate_chars(text, TOOL_BUDGET)));
                }
                _ => {}
            }
            if latest_user.is_some() && latest_tool.is_some() {
                break;
            }
        }

        let change_key = latest_user.clone().unwrap_or_else(|| fallback.to_string());
        let query = match (latest_user, latest_tool) {
            (Some(u), Some((t_name, t_snip))) => {
                format!("{}\n[recent tool: {}] {}", u, t_name, t_snip)
            }
            (Some(u), None) => u,
            (None, Some((t_name, t_snip))) => {
                format!("[recent tool: {}] {}", t_name, t_snip)
            }
            (None, None) => fallback.to_string(),
        };
        (query, change_key)
    }

    /// Truncate at a UTF-8 char boundary at or before `max_chars`.
    /// Single-pass: `take` is naturally bounded so no length pre-count is needed.
    fn truncate_chars(s: &str, max_chars: usize) -> String {
        s.chars().take(max_chars).collect()
    }

    /// Hash a string with the std DefaultHasher. Used to detect when the
    /// dynamic retrieval query has changed between iterations so we can
    /// gate the (somewhat expensive) re-classification + re-execution.
    pub(crate) fn hash_query(s: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    }

    /// Inject related scratchpad notes into knowledge blocks.
    ///
    /// Extracts topic keywords from the task prompt and recent context, searches
    /// the scratchpad for matching pages, then walks the link graph (BFS) from
    /// the top match to collect a subgraph of related notes.
    ///
    /// Returns a formatted knowledge block string, or empty string if no relevant
    /// scratchpad pages were found.
    async fn inject_scratchpad_knowledge(
        &self,
        agent_id: &AgentID,
        task_prompt: &str,
        context: &ContextWindow,
    ) -> String {
        let agent_id_str = agent_id.to_string();
        let config = &self.config.scratchpad;

        // Extract topic keywords from task prompt + recent context entries
        let keywords = Self::extract_topic_keywords(task_prompt, context);
        if keywords.is_empty() {
            return String::new();
        }

        // Search scratchpad for matching pages
        let matches = match self
            .scratchpad_store
            .search(&agent_id_str, &keywords, &[], 3)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(
                    agent_id = %agent_id_str,
                    error = %e,
                    "Scratchpad search failed during context injection"
                );
                return String::new();
            }
        };

        if matches.is_empty() {
            return String::new();
        }

        // Use the top match as seed for graph traversal
        let walker = agentos_scratch::GraphWalker::new(&self.scratchpad_store);
        let subgraph = match walker
            .subgraph(
                &agent_id_str,
                &matches[0].page.title,
                config.context_depth,
                config.max_context_pages,
                config.max_context_bytes,
            )
            .await
        {
            Ok(sg) => sg,
            Err(e) => {
                tracing::debug!(
                    agent_id = %agent_id_str,
                    seed = %matches[0].page.title,
                    error = %e,
                    "Scratchpad graph traversal failed during context injection"
                );
                return String::new();
            }
        };

        if subgraph.pages.is_empty() {
            return String::new();
        }

        // Format as a knowledge block
        let mut parts = Vec::with_capacity(subgraph.pages.len());
        for (i, page) in subgraph.pages.iter().enumerate() {
            let depth = subgraph.depths.get(i).copied().unwrap_or(0);
            parts.push(format!(
                "## {} (distance: {})\n{}",
                page.title, depth, page.content
            ));
        }

        format!(
            "[SCRATCHPAD_CONTEXT]\n{}\n[/SCRATCHPAD_CONTEXT]",
            parts.join("\n\n")
        )
    }

    /// Extract topic keywords from the task prompt and recent context entries.
    ///
    /// Simple heuristic: takes significant words (>3 chars, not stopwords)
    /// from the task prompt and last few non-system context entries.
    fn extract_topic_keywords(task_prompt: &str, context: &ContextWindow) -> String {
        const STOPWORDS: &[&str] = &[
            "the", "this", "that", "with", "from", "have", "been", "will", "would", "could",
            "should", "about", "into", "your", "what", "when", "where", "which", "there", "their",
            "they", "them", "then", "than", "these", "those", "each", "some", "also", "just",
            "more", "most", "only", "very", "does", "done", "here", "make", "made", "like", "over",
            "such", "take", "back", "well", "much", "good", "need", "want", "look", "know", "help",
            "give", "tell", "find", "work", "call", "come", "keep", "many", "long", "show", "last",
            "same", "used", "using", "please", "sure",
        ];

        let mut words: Vec<String> = Vec::new();

        // Extract from task prompt
        for word in task_prompt.split_whitespace() {
            let clean: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            let lower = clean.to_lowercase();
            if lower.len() > 3 && !STOPWORDS.contains(&lower.as_str()) {
                words.push(lower);
            }
        }

        // Extract from last 3 non-system context entries
        let recent_entries: Vec<&ContextEntry> = context
            .entries
            .iter()
            .rev()
            .filter(|e| e.role != ContextRole::System)
            .take(3)
            .collect();

        for entry in recent_entries {
            // Take first 200 chars to avoid processing huge entries
            let snippet: String = entry.text().chars().take(200).collect();
            for word in snippet.split_whitespace() {
                let clean: String = word
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                let lower = clean.to_lowercase();
                if lower.len() > 3 && !STOPWORDS.contains(&lower.as_str()) {
                    words.push(lower);
                }
            }
        }

        // Deduplicate and limit, then quote each keyword for FTS5 safety
        // (prevents `-` being interpreted as NOT operator, etc.)
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<String> = words
            .into_iter()
            .filter(|w| seen.insert(w.clone()))
            .take(10)
            .collect();

        unique
            .iter()
            .map(|w| format!("\"{}\"", w))
            .collect::<Vec<_>>()
            .join(" OR ")
    }

    /// Build the agent-facing tool-not-found error payload, write the
    /// `ToolSuggested` audit row, and increment `suggest_count`. Both the
    /// parallel-batch path and the sequential dispatcher call this so
    /// the audit shape and error wording stay in sync (review R2 S5).
    ///
    /// Returns `serde_json::Value` (the error_result) for the caller to
    /// push into the agent's context.
    pub(crate) async fn build_tool_not_found_payload(
        &self,
        tool_name: &str,
        task_id: TaskID,
        agent_id: AgentID,
        trace_id: TraceID,
        suggest_count: &mut u32,
    ) -> serde_json::Value {
        // Compute suggestions FIRST, then increment the cap counter
        // only if we actually produced something. Without this guard,
        // a string of unknown tool names with no near-matches would
        // burn the cap silently, suppressing later legitimate hints
        // (review fix #1).
        let (suggestions, sections) = if *suggest_count < 3 {
            let summaries = {
                let g = self.tool_summaries.read().await;
                g.clone()
            };
            (
                suggest_tools(&summaries, tool_name, 3),
                agentos_tools::suggest_manual_sections_async(tool_name, 2).await,
            )
        } else {
            (vec![], vec![])
        };
        if !suggestions.is_empty() || !sections.is_empty() {
            *suggest_count += 1;
        }

        if !suggestions.is_empty() || !sections.is_empty() {
            self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id,
                event_type: agentos_audit::AuditEventType::ToolSuggested,
                agent_id: Some(agent_id),
                task_id: Some(task_id),
                tool_id: None,
                details: serde_json::json!({
                    "missing_tool": tool_name,
                    "suggestions": suggestions,
                    "manual_sections": sections,
                    "task_suggest_count": *suggest_count,
                }),
                severity: agentos_audit::AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            });
        }

        if suggestions.is_empty() && sections.is_empty() {
            return serde_json::json!({
                "error": format!("Unknown tool requested: {}", tool_name),
            });
        }

        let mut msg = format!("Tool '{}' not found.", tool_name);
        if !suggestions.is_empty() {
            msg.push_str(&format!(" Did you mean: {}?", suggestions.join(", ")));
        }
        // Inline a one-line summary per suggested section so a small
        // model can resolve the next move without a round-trip
        // `agent-manual section=X` call. Each summary is bounded to
        // ~140 chars by `ManualSection::section_summary`, so 2 sections
        // add at most ~300 chars to the error payload — well below the
        // tool-output cap.
        let briefs: Vec<String> = sections
            .iter()
            .filter_map(|n| {
                agentos_tools::agent_manual::ManualSection::section_summary(n)
                    .map(|s| format!("  - {n}: {s}"))
            })
            .collect();
        if !briefs.is_empty() {
            msg.push_str("\nRelevant manual sections:\n");
            msg.push_str(&briefs.join("\n"));
            msg.push_str(&format!(
                "\n(Full prose via `agent-manual section=<name>` for: {}.)",
                sections.join(", ")
            ));
        }
        msg.push_str("\nUse `list-tools` or `search-tools` to discover available tools.");
        serde_json::json!({ "error": msg })
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
/// Drop old ToolResult entries for idempotent meta-tools (list-tools, search-tools).
/// Keeps only the latest result per tool — older ones are replaced with a one-line placeholder
/// so the paired assistant tool_use block stays valid (Anthropic API requires tool_use+tool_result pairs).
fn scrub_meta_tool_results(history: &mut [ContextEntry]) {
    const META_TOOLS: &[&str] = &["list-tools", "search-tools"];

    // Find the index of the last ToolResult entry for each meta tool.
    let mut latest: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, entry) in history.iter().enumerate() {
        if entry.role == ContextRole::ToolResult {
            if let Some(ref meta) = entry.metadata {
                if let Some(ref name) = meta.tool_name {
                    if let Some(&mt) = META_TOOLS.iter().find(|&&mt| mt == name.as_str()) {
                        latest.insert(mt, i);
                    }
                }
            }
        }
    }

    // Replace content of all older ToolResult entries for meta tools with a short placeholder.
    for (i, entry) in history.iter_mut().enumerate() {
        if entry.role == ContextRole::ToolResult {
            if let Some(ref meta) = entry.metadata {
                if let Some(ref name) = meta.tool_name {
                    if META_TOOLS.contains(&name.as_str()) {
                        let is_latest = latest.get(name.as_str()).copied() == Some(i);
                        if !is_latest {
                            entry.parts = vec![ContentPart::Text {
                                text:
                                    r#"{"replaced":"Stale result — superseded by a newer call."}"#
                                        .to_string(),
                            }];
                        }
                    }
                }
            }
        }
    }
}

fn should_sandbox_tool(policy: crate::config::SandboxPolicy, trust_tier: TrustTier) -> bool {
    match policy {
        crate::config::SandboxPolicy::Never => false,
        crate::config::SandboxPolicy::Always => true,
        crate::config::SandboxPolicy::TrustAware => trust_tier != TrustTier::Core,
    }
}

/// Return up to `max` tool names from `summaries` closest to `query`.
/// Scores: substring containment (strong), then bigram overlap (order-sensitive).
fn suggest_tools(
    summaries: &[agentos_tools::agent_manual::ToolSummary],
    query: &str,
    max: usize,
) -> Vec<String> {
    let q = query.to_lowercase();
    let q_grams: std::collections::HashSet<[u8; 2]> =
        q.as_bytes().windows(2).map(|w| [w[0], w[1]]).collect();

    let mut scored: Vec<(i32, &str)> = summaries
        .iter()
        .map(|s| {
            let name = s.name.to_lowercase();
            let mut score: i32 = 0;
            if name.contains(q.as_str()) {
                score += 100;
            } else if q.contains(name.as_str()) {
                score += 50;
            }
            let n_grams: std::collections::HashSet<[u8; 2]> =
                name.as_bytes().windows(2).map(|w| [w[0], w[1]]).collect();
            score += q_grams.intersection(&n_grams).count() as i32;
            (score, s.name.as_str())
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(max)
        .map(|(_, n)| n.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    #[test]
    fn extract_approval_pending_id_parses_structured_reason() {
        assert_eq!(
            super::extract_approval_pending_id(
                "approval_pending:42: tool 'host-package-install' …"
            ),
            Some(42)
        );
    }

    #[test]
    fn extract_approval_pending_id_rejects_unstructured() {
        // Plain hook denial — no pending channel installed.
        assert!(super::extract_approval_pending_id("Blocked by hook: bad input").is_none());
        // Wrong prefix.
        assert!(super::extract_approval_pending_id("approval:42:foo").is_none());
        // Missing id.
        assert!(super::extract_approval_pending_id("approval_pending::nope").is_none());
        // Non-numeric id.
        assert!(super::extract_approval_pending_id("approval_pending:abc:nope").is_none());
    }

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
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: Default::default(),
            spawner_agent_id: None,
            tool_categories: None,
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

    // ── extract_topic_keywords tests ──────────────────────────────────────

    fn make_context_entry(role: ContextRole, text: &str) -> ContextEntry {
        ContextEntry {
            role,
            parts: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        }
    }

    #[test]
    fn extract_keywords_filters_stopwords() {
        let ctx = ContextWindow::new(10);
        let result =
            Kernel::extract_topic_keywords("the quick brown foxes jumped over the lazy dogs", &ctx);
        // "the", "over" are stopwords; "quick", "brown", "foxes", "jumped", "lazy", "dogs" should survive
        assert!(!result.contains("\"the\""));
        assert!(!result.contains("\"over\""));
        assert!(result.contains("\"quick\""));
        assert!(result.contains("\"brown\""));
        assert!(result.contains("\"foxes\""));
    }

    #[test]
    fn extract_keywords_filters_short_words() {
        let ctx = ContextWindow::new(10);
        let result = Kernel::extract_topic_keywords("a is an do go API key", &ctx);
        // Words ≤ 3 chars should be excluded (except they'd also be stopwords)
        assert!(!result.contains("\"is\""));
        assert!(!result.contains("\"an\""));
    }

    #[test]
    fn extract_keywords_deduplicates() {
        let ctx = ContextWindow::new(10);
        let result = Kernel::extract_topic_keywords("scratchpad scratchpad scratchpad notes", &ctx);
        // "scratchpad" should appear only once
        let count = result.matches("\"scratchpad\"").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn extract_keywords_limits_to_10() {
        let ctx = ContextWindow::new(10);
        let prompt = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november";
        let result = Kernel::extract_topic_keywords(prompt, &ctx);
        let keyword_count = result.matches(" OR ").count() + 1;
        assert!(
            keyword_count <= 10,
            "Expected ≤ 10 keywords, got {}",
            keyword_count
        );
    }

    #[test]
    fn extract_keywords_includes_context_entries() {
        let mut ctx = ContextWindow::new(10);
        ctx.push(make_context_entry(ContextRole::System, "system prompt"));
        ctx.push(make_context_entry(
            ContextRole::User,
            "deploy kubernetes cluster",
        ));
        ctx.push(make_context_entry(
            ContextRole::Assistant,
            "checking cluster status",
        ));

        let result = Kernel::extract_topic_keywords("scaling pods", &ctx);
        // Should include words from recent non-system entries
        assert!(
            result.contains("\"kubernetes\"")
                || result.contains("\"cluster\"")
                || result.contains("\"deploy\"")
        );
        // Should NOT include system prompt content
        // (system entries are filtered out)
    }

    #[test]
    fn extract_keywords_returns_empty_for_empty_input() {
        let ctx = ContextWindow::new(10);
        let result = Kernel::extract_topic_keywords("", &ctx);
        assert!(result.is_empty());
    }

    #[test]
    fn extract_keywords_quotes_for_fts5() {
        let ctx = ContextWindow::new(10);
        let result = Kernel::extract_topic_keywords("error-handling patterns", &ctx);
        // Each keyword should be double-quoted and joined with OR
        assert!(result.contains("\"error-handling\""));
        assert!(result.contains(" OR "));
    }

    // ── build_turn_reminder tests ─────────────────────────────────────────

    fn make_tool_result_entry(tool_name: &str, body: &str) -> ContextEntry {
        ContextEntry {
            role: ContextRole::ToolResult,
            parts: vec![ContentPart::Text {
                text: body.to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: Some(agentos_types::ContextMetadata {
                tool_name: Some(tool_name.to_string()),
                tool_id: None,
                intent_id: None,
                tokens_estimated: None,
                tool_call_id: None,
                assistant_tool_calls: None,
            }),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        }
    }

    fn make_test_task() -> AgentTask {
        AgentTask {
            max_iterations: Some(40),
            ..AgentTask::default()
        }
    }

    #[test]
    fn turn_reminder_includes_turn_count_and_elapsed() {
        let task = make_test_task();
        let ctx = ContextWindow::new(10);
        let r = Kernel::build_turn_reminder(
            &task,
            4,
            7,
            std::time::Duration::from_millis(12_345),
            &ctx,
        );
        assert!(r.contains("turn: 5/40"));
        assert!(r.contains("tool_calls: 7"));
        assert!(r.contains("elapsed: 12.3"));
        assert!(r.contains("<turn_reminder>"));
        assert!(r.contains("</turn_reminder>"));
    }

    #[test]
    fn turn_reminder_lists_last_three_tools_chronological() {
        let task = make_test_task();
        let mut ctx = ContextWindow::new(20);
        ctx.entries
            .push(make_tool_result_entry("file-read", r#"{"content":"hi"}"#));
        ctx.entries
            .push(make_tool_result_entry("memory-search", r#"{"results":[]}"#));
        ctx.entries.push(make_tool_result_entry(
            "shell-exec",
            r#"{"exit_code":1,"error":"boom"}"#,
        ));
        ctx.entries
            .push(make_tool_result_entry("file-write", r#"{"bytes":42}"#));
        let r = Kernel::build_turn_reminder(&task, 0, 4, std::time::Duration::ZERO, &ctx);
        // Recent line should show the LAST 3 (memory-search, shell-exec, file-write)
        // in chronological order, NOT include file-read.
        let recent_line = r
            .lines()
            .find(|l| l.starts_with("recent:"))
            .expect("recent line present");
        assert!(recent_line.contains("memory-search[ok]"));
        assert!(recent_line.contains("shell-exec[fail]"));
        assert!(recent_line.contains("file-write[ok]"));
        assert!(!recent_line.contains("file-read"));
        // Order check: memory-search before shell-exec before file-write
        let i_mem = recent_line.find("memory-search").unwrap();
        let i_shell = recent_line.find("shell-exec").unwrap();
        let i_write = recent_line.find("file-write").unwrap();
        assert!(i_mem < i_shell && i_shell < i_write);
    }

    #[test]
    fn turn_reminder_handles_empty_context() {
        let task = make_test_task();
        let ctx = ContextWindow::new(10);
        let r = Kernel::build_turn_reminder(&task, 0, 0, std::time::Duration::ZERO, &ctx);
        assert!(r.contains("(no tools called yet)"));
    }

    #[test]
    fn turn_reminder_unknown_max_iterations_renders_question_mark() {
        let task = AgentTask {
            max_iterations: None,
            ..AgentTask::default()
        };
        let ctx = ContextWindow::new(10);
        let r = Kernel::build_turn_reminder(&task, 0, 0, std::time::Duration::ZERO, &ctx);
        assert!(r.contains("turn: 1/?"));
    }

    // ── build_dynamic_retrieval_query tests ───────────────────────────────

    #[test]
    fn dynamic_query_falls_back_when_context_empty() {
        let ctx = ContextWindow::new(10);
        let (q, k) = Kernel::build_dynamic_retrieval_query(&ctx, "fallback query");
        assert_eq!(q, "fallback query");
        assert_eq!(k, "fallback query");
    }

    #[test]
    fn dynamic_query_uses_latest_user_only() {
        let mut ctx = ContextWindow::new(10);
        ctx.entries
            .push(make_context_entry(ContextRole::User, "first message"));
        ctx.entries
            .push(make_context_entry(ContextRole::Assistant, "I'll handle it"));
        ctx.entries.push(make_context_entry(
            ContextRole::User,
            "now switch to the database",
        ));
        let (q, _k) = Kernel::build_dynamic_retrieval_query(&ctx, "fallback");
        assert_eq!(q, "now switch to the database");
        assert!(!q.contains("first message"));
    }

    #[test]
    fn dynamic_query_combines_user_and_tool_result() {
        let mut ctx = ContextWindow::new(10);
        ctx.entries.push(make_context_entry(
            ContextRole::User,
            "look at the auth flow",
        ));
        ctx.entries.push(make_tool_result_entry(
            "file-read",
            r#"{"content":"fn login() { ... }"}"#,
        ));
        let (q, _k) = Kernel::build_dynamic_retrieval_query(&ctx, "fallback");
        assert!(q.contains("look at the auth flow"));
        assert!(q.contains("[recent tool: file-read]"));
        assert!(q.contains("login"));
    }

    #[test]
    fn dynamic_query_truncates_long_user_input() {
        let mut ctx = ContextWindow::new(10);
        let long_msg = "a".repeat(2000);
        ctx.entries
            .push(make_context_entry(ContextRole::User, &long_msg));
        let (q, _k) = Kernel::build_dynamic_retrieval_query(&ctx, "fallback");
        // Capped at 500 chars per the helper's USER_BUDGET.
        assert!(q.chars().count() <= 500);
    }

    #[test]
    fn dynamic_query_change_key_excludes_tool_snippet() {
        // Phase 2 W2 fix: rotating tool results must NOT trip the change-key.
        // Same user message, different tool snippets → identical change_key.
        let mut ctx_a = ContextWindow::new(10);
        ctx_a
            .entries
            .push(make_context_entry(ContextRole::User, "fix the auth flow"));
        ctx_a
            .entries
            .push(make_tool_result_entry("file-read", r#"{"content":"AAA"}"#));

        let mut ctx_b = ContextWindow::new(10);
        ctx_b
            .entries
            .push(make_context_entry(ContextRole::User, "fix the auth flow"));
        ctx_b
            .entries
            .push(make_tool_result_entry("file-read", r#"{"content":"BBB"}"#));

        let (qa, ka) = Kernel::build_dynamic_retrieval_query(&ctx_a, "fallback");
        let (qb, kb) = Kernel::build_dynamic_retrieval_query(&ctx_b, "fallback");
        // Full queries differ (snippet content rotates) ...
        assert_ne!(qa, qb);
        // ... but the change-key is identical (same user message).
        assert_eq!(ka, kb);
    }

    #[test]
    fn hash_query_changes_with_content() {
        let h1 = Kernel::hash_query("look at the database");
        let h2 = Kernel::hash_query("look at the auth flow");
        assert_ne!(h1, h2);
        // And stable for identical input.
        assert_eq!(h1, Kernel::hash_query("look at the database"));
    }

    // ── wrap_with_manifest_meta tests ─────────────────────────────────────

    #[test]
    fn wrap_meta_returns_unchanged_when_hints_absent() {
        let raw = serde_json::json!({"output": "hello"});
        let out = Kernel::wrap_with_manifest_meta(raw.clone(), "any-tool", None);
        assert_eq!(out, raw);
    }

    #[test]
    fn wrap_meta_returns_unchanged_when_hints_all_empty() {
        let raw = serde_json::json!({"output": "hello"});
        let hints = agentos_types::UsageHints::default();
        let out = Kernel::wrap_with_manifest_meta(raw.clone(), "any-tool", Some(&hints));
        assert_eq!(out, raw);
    }

    #[test]
    fn wrap_meta_envelopes_when_related_tools_present() {
        let raw = serde_json::json!({"output": "hello"});
        let hints = agentos_types::UsageHints {
            use_for: vec!["read existing file".to_string()],
            prefer_over: vec![],
            quick_example: None,
            related_tools: vec!["file-writer".to_string(), "file-edit".to_string()],
        };
        let out = Kernel::wrap_with_manifest_meta(raw.clone(), "file-reader", Some(&hints));
        // Wrapped under "result" + "_meta"
        assert_eq!(out["result"], raw);
        assert_eq!(out["_meta"]["tool"], "file-reader");
        assert_eq!(out["_meta"]["use_for"][0], "read existing file");
        assert_eq!(out["_meta"]["related_tools"][0], "file-writer");
        assert_eq!(out["_meta"]["related_tools"][1], "file-edit");
        // prefer_over was empty — should not be in meta
        assert!(out["_meta"].get("prefer_over").is_none());
    }
}
