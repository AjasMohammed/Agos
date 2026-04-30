use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_types::*;
use chrono::Utc;
use std::collections::HashMap;
use std::time::Duration;

/// Actions that tools can request the kernel to perform on their behalf.
///
/// Tools return a `_kernel_action` field in their result JSON to signal
/// that the kernel should intercept the result and perform a privileged
/// operation (e.g., delegating a task, sending an inter-agent message).
/// This keeps tools stateless while the kernel retains full control over
/// scheduling, permissions, and audit.
#[derive(Debug)]
pub(crate) enum KernelAction {
    DelegateTask {
        target_agent: String,
        prompt: String,
        priority: u8,
    },
    SendAgentMessage {
        to: String,
        content: String,
    },
    EscalateToHuman {
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        urgency: String,
        blocking: bool,
    },
    SwitchPartition {
        partition: String, // "active" or "scratchpad"
    },
    MemoryBlockWrite {
        label: String,
        content: String,
    },
    MemoryBlockRead {
        label: String,
    },
    MemoryBlockList,
    MemoryBlockDelete {
        label: String,
    },
    /// Fire-and-forget notification to the user inbox.
    NotifyUser {
        subject: String,
        body: String,
        priority: String,
    },
    /// Blocking question to the user — task pauses until user responds.
    AskUser {
        question: String,
        options: Option<Vec<String>>,
        timeout_secs: u64,
        priority: String,
        auto_action: String,
    },
    /// Synchronous agent-to-agent RPC call — blocks until the target agent
    /// completes the child task and returns its output.
    AgentRpcCall {
        target_agent: String,
        prompt: String,
        timeout_secs: u64,
    },
    /// Update the agent's self-curated context memory document.
    ContextMemoryUpdate {
        agent_id: String,
        content: String,
        reason: Option<String>,
    },
    /// Read the agent's current context memory document.
    ContextMemoryRead {
        agent_id: String,
    },
    /// Spawn a sub-agent task scoped to the current task's capabilities.
    SpawnAgent {
        agent: String,
        prompt: String,
        permissions: Vec<String>,
        context_messages: u64,
    },
    /// Wait for spawned sub-agent tasks and collect their results.
    AwaitAgents {
        task_ids: Vec<String>,
    },
    /// Non-blocking poll of spawned sub-agent status and progress.
    PollAgents {
        task_ids: Vec<String>,
        include_progress: bool,
    },
    /// Cancel a spawned sub-agent (cascades to grandchildren).
    CancelAgent {
        task_id: String,
        reason: String,
    },
    /// Fire-and-forget async spawn. Creates a child task but does NOT add a
    /// scheduler dependency, so the parent continues immediately. On completion
    /// the child's result is injected into the spawner's context window.
    SpawnAsync {
        target_agent: String,
        prompt: String,
        priority: u8,
    },
    /// Delegate a task to an external A2A-compliant agent via HTTP.
    A2ADelegate {
        agent_url: String,
        capability: String,
        input: serde_json::Value,
        token: Option<String>,
        wait_for_result: bool,
    },
    /// Subscribe the calling agent to events matching a filter.
    /// Permission-gated per `EventCategory` via `event_permissions`.
    EventSubscribeAction {
        event_filter: String,
        payload_filter: Option<String>,
        throttle: Option<String>,
        priority: Option<String>,
    },
    /// Cancel one of the calling agent's own subscriptions by ID.
    EventUnsubscribeAction {
        subscription_id: String,
    },
    /// Return all subscriptions belonging to the calling agent.
    EventListSubscriptionsAction,
    /// Enumerate all event categories and types, marking which ones the
    /// calling agent currently has permission to subscribe to.
    EventListAvailableAction,
    /// Create an in-memory one-shot timer that fires after `delay_secs`.
    SetTimer {
        name: String,
        delay_secs: u64,
        agent_name: String,
        action: TimerAction,
    },
    /// Cancel a pending in-memory timer by name.
    CancelTimer {
        name: String,
    },
    /// List all pending in-memory timers.
    ListTimers,
    /// Schedule a one-shot task at an absolute datetime (or a relative delay).
    ScheduleOnce {
        name: String,
        task_prompt: String,
        agent_name: String,
        fire_at: chrono::DateTime<chrono::Utc>,
    },
    /// Cancel a pending once-job by name.
    CancelOnceJob {
        name: String,
    },
    /// List all pending once-jobs.
    ListOnceJobs,
    /// Send a message to a single connected channel by name or ID.
    /// Distinct from NotifyUser, which fans out to every registered delivery
    /// adapter. ChannelSend is targeted: agent picks one channel.
    ChannelSend {
        /// Display name (e.g. "telegram-main") or `ChannelInstanceID` UUID.
        channel: String,
        /// Message body. Markdown is rendered per-platform when supported.
        text: String,
        /// Optional thread/reply target (platform-specific).
        thread_id: Option<String>,
    },
}

/// Why an agent is requesting human escalation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EscalationReason {
    /// Agent is uncertain about the correct approach.
    Uncertainty,
    /// Agent detected a potential safety or security concern.
    SafetyConcern,
    /// Agent needs authorization for a high-impact action.
    AuthorizationRequired,
    /// Agent encountered an ambiguous or contradictory instruction.
    AmbiguousInstruction,
    /// Custom reason not covered above.
    Other(String),
}

/// Outcome of executing a kernel action, used to build the tool result
/// that gets pushed into the agent's context.
pub(crate) struct KernelActionResult {
    pub success: bool,
    pub result: serde_json::Value,
}

impl KernelAction {
    /// Try to parse a kernel action from a tool result.
    /// Returns `None` if the result does not contain a `_kernel_action` field.
    pub fn from_tool_result(value: &serde_json::Value) -> Option<Self> {
        let action = value.get("_kernel_action")?.as_str()?;
        match action {
            "delegate_task" => {
                let target_agent = value.get("target_agent")?.as_str()?.to_string();
                let prompt = value.get("task")?.as_str()?.to_string();
                let priority = value.get("priority").and_then(|v| v.as_u64()).unwrap_or(5) as u8;
                Some(Self::DelegateTask {
                    target_agent,
                    prompt,
                    priority,
                })
            }
            "send_agent_message" => {
                let to = value.get("to")?.as_str()?.to_string();
                let content = value.get("content")?.as_str()?.to_string();
                Some(Self::SendAgentMessage { to, content })
            }
            "escalate" => {
                let reason_str = value
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("other");
                let reason = match reason_str {
                    "uncertainty" => EscalationReason::Uncertainty,
                    "safety_concern" => EscalationReason::SafetyConcern,
                    "authorization_required" => EscalationReason::AuthorizationRequired,
                    "ambiguous_instruction" => EscalationReason::AmbiguousInstruction,
                    other => EscalationReason::Other(other.to_string()),
                };
                let context_summary = value
                    .get("context_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let decision_point = value
                    .get("decision_point")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let options = value
                    .get("options")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let urgency = value
                    .get("urgency")
                    .and_then(|v| v.as_str())
                    .unwrap_or("normal")
                    .to_string();
                let blocking = value
                    .get("blocking")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                Some(Self::EscalateToHuman {
                    reason,
                    context_summary,
                    decision_point,
                    options,
                    urgency,
                    blocking,
                })
            }
            "switch_partition" => {
                let partition = value
                    .get("partition")
                    .and_then(|v| v.as_str())
                    .unwrap_or("active")
                    .to_string();
                Some(Self::SwitchPartition { partition })
            }
            "memory_block_write" => {
                let label = value.get("label")?.as_str()?.to_string();
                let content = value.get("content")?.as_str()?.to_string();
                Some(Self::MemoryBlockWrite { label, content })
            }
            "memory_block_read" => {
                let label = value.get("label")?.as_str()?.to_string();
                Some(Self::MemoryBlockRead { label })
            }
            "memory_block_list" => Some(Self::MemoryBlockList),
            "memory_block_delete" => {
                let label = value.get("label")?.as_str()?.to_string();
                Some(Self::MemoryBlockDelete { label })
            }
            "notify_user" => {
                let subject = value.get("subject")?.as_str()?.to_string();
                let body = value.get("body")?.as_str()?.to_string();
                let priority = value
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_string();
                Some(Self::NotifyUser {
                    subject,
                    body,
                    priority,
                })
            }
            "channel_send" => {
                let channel = value.get("channel")?.as_str()?;
                let text = value.get("text")?.as_str()?;
                if channel.trim().is_empty() || text.is_empty() {
                    tracing::warn!("Dropping channel_send: channel/text must be non-empty");
                    return None;
                }
                let thread_id = value
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                Some(Self::ChannelSend {
                    channel: channel.to_string(),
                    text: text.to_string(),
                    thread_id,
                })
            }
            "ask_user" => {
                let question = value.get("question")?.as_str()?.to_string();
                let options = value.get("options").and_then(|v| v.as_array()).map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                });
                let timeout_secs = value
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300);
                let priority = value
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_string();
                let auto_action = value
                    .get("auto_action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto_denied")
                    .to_string();
                Some(Self::AskUser {
                    question,
                    options,
                    timeout_secs,
                    priority,
                    auto_action,
                })
            }
            "agent_rpc_call" => {
                let target_agent = value.get("target_agent")?.as_str()?.to_string();
                let prompt = value.get("prompt")?.as_str()?.to_string();
                let timeout_secs = value
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300);
                Some(Self::AgentRpcCall {
                    target_agent,
                    prompt,
                    timeout_secs,
                })
            }
            "context_memory_update" => {
                let agent_id = value.get("agent_id")?.as_str()?.to_string();
                let content = value.get("content")?.as_str()?.to_string();
                let reason = value
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Some(Self::ContextMemoryUpdate {
                    agent_id,
                    content,
                    reason,
                })
            }
            "context_memory_read" => {
                let agent_id = value.get("agent_id")?.as_str()?.to_string();
                Some(Self::ContextMemoryRead { agent_id })
            }
            "spawn_agent" => {
                let agent = value.get("agent")?.as_str()?.to_string();
                let prompt = value.get("prompt")?.as_str()?.to_string();
                let permissions = value
                    .get("permissions")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let context_messages = value
                    .get("context_messages")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10);
                Some(Self::SpawnAgent {
                    agent,
                    prompt,
                    permissions,
                    context_messages,
                })
            }
            "await_agents" => {
                let task_ids = value
                    .get("task_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(Self::AwaitAgents { task_ids })
            }
            "poll_agents" => {
                let task_ids = value
                    .get("task_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let include_progress = value
                    .get("include_progress")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                Some(Self::PollAgents {
                    task_ids,
                    include_progress,
                })
            }
            "cancel_agent" => {
                let task_id = value.get("task_id")?.as_str()?.to_string();
                let reason = value
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Cancelled by parent agent")
                    .to_string();
                Some(Self::CancelAgent { task_id, reason })
            }
            "spawn_async" => {
                let target_agent = value.get("target_agent")?.as_str()?.to_string();
                let prompt = value.get("task")?.as_str()?.to_string();
                let priority = value.get("priority").and_then(|v| v.as_u64()).unwrap_or(5) as u8;
                Some(Self::SpawnAsync {
                    target_agent,
                    prompt,
                    priority,
                })
            }
            "a2a_delegate" => {
                let agent_url = value.get("agent_url")?.as_str()?.to_string();
                let capability = value.get("capability")?.as_str()?.to_string();
                let input = value.get("input").cloned().unwrap_or(serde_json::json!({}));
                let token = value
                    .get("token")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let wait_for_result = value
                    .get("wait_for_result")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some(Self::A2ADelegate {
                    agent_url,
                    capability,
                    input,
                    token,
                    wait_for_result,
                })
            }
            "event_subscribe" => {
                let event_filter = value.get("event_filter")?.as_str()?.to_string();
                let payload_filter = value
                    .get("payload_filter")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let throttle = value
                    .get("throttle")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let priority = value
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                Some(Self::EventSubscribeAction {
                    event_filter,
                    payload_filter,
                    throttle,
                    priority,
                })
            }
            "event_unsubscribe" => {
                let subscription_id = value.get("subscription_id")?.as_str()?.to_string();
                Some(Self::EventUnsubscribeAction { subscription_id })
            }
            "event_list_subscriptions" => Some(Self::EventListSubscriptionsAction),
            "event_list_available" => Some(Self::EventListAvailableAction),
            "set_timer" => {
                let name = value.get("name")?.as_str()?.to_string();
                let delay_secs = value.get("delay_secs")?.as_u64()?;
                let agent_name = value
                    .get("agent_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let action: TimerAction = match serde_json::from_value(value.get("action")?.clone())
                {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!(error = %e, "set_timer: failed to deserialize TimerAction");
                        return None;
                    }
                };
                Some(Self::SetTimer {
                    name,
                    delay_secs,
                    agent_name,
                    action,
                })
            }
            "cancel_timer" => {
                let name = value.get("name")?.as_str()?.to_string();
                Some(Self::CancelTimer { name })
            }
            "list_timers" => Some(Self::ListTimers),
            "schedule_once" => {
                let name = value.get("name")?.as_str()?.to_string();
                let task_prompt = value.get("task_prompt")?.as_str()?.to_string();
                let agent_name = value
                    .get("agent_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let fire_at: chrono::DateTime<chrono::Utc> = match serde_json::from_value(
                    value.get("fire_at")?.clone(),
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(error = %e, "schedule_once: failed to deserialize fire_at");
                        return None;
                    }
                };
                Some(Self::ScheduleOnce {
                    name,
                    task_prompt,
                    agent_name,
                    fire_at,
                })
            }
            "cancel_once_job" => {
                let name = value.get("name")?.as_str()?.to_string();
                Some(Self::CancelOnceJob { name })
            }
            "list_once_jobs" => Some(Self::ListOnceJobs),
            other => {
                tracing::warn!(action = %other, "Unknown _kernel_action, ignoring");
                None
            }
        }
    }
}

impl Kernel {
    /// Execute a kernel action on behalf of a running task.
    ///
    /// This is the central dispatch point for all tool-initiated kernel
    /// operations. It enforces permissions via the existing capability
    /// system and produces full audit trails.
    pub(crate) async fn dispatch_kernel_action(
        &self,
        task: &AgentTask,
        action: KernelAction,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let action_name = match &action {
            KernelAction::DelegateTask { .. } => "delegate_task",
            KernelAction::SendAgentMessage { .. } => "send_agent_message",
            KernelAction::EscalateToHuman { .. } => "escalate",
            KernelAction::SwitchPartition { .. } => "switch_partition",
            KernelAction::MemoryBlockWrite { .. } => "memory_block_write",
            KernelAction::MemoryBlockRead { .. } => "memory_block_read",
            KernelAction::MemoryBlockList => "memory_block_list",
            KernelAction::MemoryBlockDelete { .. } => "memory_block_delete",
            KernelAction::NotifyUser { .. } => "notify_user",
            KernelAction::AskUser { .. } => "ask_user",
            KernelAction::AgentRpcCall { .. } => "agent_rpc_call",
            KernelAction::ContextMemoryUpdate { .. } => "context_memory_update",
            KernelAction::ContextMemoryRead { .. } => "context_memory_read",
            KernelAction::SpawnAgent { .. } => "spawn_agent",
            KernelAction::AwaitAgents { .. } => "await_agents",
            KernelAction::PollAgents { .. } => "poll_agents",
            KernelAction::CancelAgent { .. } => "cancel_agent",
            KernelAction::SpawnAsync { .. } => "spawn_async",
            KernelAction::A2ADelegate { .. } => "a2a_delegate",
            KernelAction::EventSubscribeAction { .. } => "event_subscribe",
            KernelAction::EventUnsubscribeAction { .. } => "event_unsubscribe",
            KernelAction::EventListSubscriptionsAction => "event_list_subscriptions",
            KernelAction::EventListAvailableAction => "event_list_available",
            KernelAction::SetTimer { .. } => "set_timer",
            KernelAction::CancelTimer { .. } => "cancel_timer",
            KernelAction::ListTimers => "list_timers",
            KernelAction::ScheduleOnce { .. } => "schedule_once",
            KernelAction::CancelOnceJob { .. } => "cancel_once_job",
            KernelAction::ListOnceJobs => "list_once_jobs",
            KernelAction::ChannelSend { .. } => "channel_send",
        };

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({ "kernel_action": action_name }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        let result = match action {
            KernelAction::DelegateTask {
                target_agent,
                prompt,
                priority,
            } => {
                self.execute_delegate_task(task, &target_agent, &prompt, priority)
                    .await
            }
            KernelAction::SendAgentMessage { to, content } => {
                self.execute_send_message(task, &to, &content, trace_id)
                    .await
            }
            KernelAction::EscalateToHuman {
                reason,
                context_summary,
                decision_point,
                options,
                urgency,
                blocking,
            } => {
                self.execute_escalation(
                    task,
                    reason,
                    &context_summary,
                    &decision_point,
                    &options,
                    &urgency,
                    blocking,
                    trace_id,
                )
                .await
            }
            KernelAction::SwitchPartition { partition } => {
                self.execute_switch_partition(task, &partition).await
            }
            KernelAction::MemoryBlockWrite { label, content } => {
                self.execute_memory_block_write(task, &label, &content)
                    .await
            }
            KernelAction::MemoryBlockRead { label } => {
                self.execute_memory_block_read(task, &label).await
            }
            KernelAction::MemoryBlockList => self.execute_memory_block_list(task).await,
            KernelAction::MemoryBlockDelete { label } => {
                self.execute_memory_block_delete(task, &label).await
            }
            KernelAction::NotifyUser {
                subject,
                body,
                priority,
            } => {
                self.execute_notify_user(task, subject, body, priority, trace_id)
                    .await
            }
            KernelAction::AskUser {
                question,
                options,
                timeout_secs,
                priority,
                auto_action,
            } => {
                self.execute_ask_user(
                    task,
                    question,
                    options,
                    timeout_secs,
                    priority,
                    auto_action,
                    trace_id,
                )
                .await
            }
            KernelAction::AgentRpcCall {
                target_agent,
                prompt,
                timeout_secs,
            } => {
                self.execute_agent_rpc_call(task, &target_agent, &prompt, timeout_secs, trace_id)
                    .await
            }
            KernelAction::ContextMemoryUpdate {
                agent_id,
                content,
                reason,
            } => {
                // Injection scanning (spec §9)
                let scan = self.injection_scanner.scan(&content);
                if scan.max_threat == Some(crate::injection_scanner::ThreatLevel::High) {
                    self.audit_log(AuditEntry {
                        timestamp: Utc::now(),
                        trace_id,
                        event_type: AuditEventType::RiskEscalation,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "source": "context_memory_update",
                            "threat": "high",
                            "agent_id": agent_id,
                        }),
                        severity: AuditSeverity::Security,
                        reversible: false,
                        rollback_ref: None,
                    });
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": "Content rejected: high-confidence injection pattern detected.",
                        }),
                    };
                }

                match self
                    .context_memory_store
                    .write(&agent_id, &content, reason.as_deref())
                    .await
                {
                    Ok(entry) => {
                        self.audit_log(AuditEntry {
                            timestamp: Utc::now(),
                            trace_id,
                            event_type: AuditEventType::ContextMemoryUpdated,
                            agent_id: Some(task.agent_id),
                            task_id: Some(task.id),
                            tool_id: None,
                            details: serde_json::json!({
                                "agent_id": entry.agent_id,
                                "version": entry.version,
                                "token_count": entry.token_count,
                                "reason": reason,
                            }),
                            severity: AuditSeverity::Info,
                            reversible: true,
                            rollback_ref: Some(format!(
                                "context_memory:{}:{}",
                                entry.agent_id,
                                entry.version.saturating_sub(1)
                            )),
                        });
                        KernelActionResult {
                            success: true,
                            result: serde_json::json!({
                                "updated": true,
                                "version": entry.version,
                                "token_count": entry.token_count,
                                "message": "Context memory updated. Changes take effect on your next task.",
                            }),
                        }
                    }
                    Err(e) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": e.to_string(),
                        }),
                    },
                }
            }
            KernelAction::ContextMemoryRead { agent_id } => {
                match self.context_memory_store.read(&agent_id).await {
                    Ok(Some(entry)) => KernelActionResult {
                        success: true,
                        result: serde_json::json!({
                            "content": entry.content,
                            "version": entry.version,
                            "token_count": entry.token_count,
                            "updated_at": entry.updated_at.to_rfc3339(),
                        }),
                    },
                    Ok(None) => KernelActionResult {
                        success: true,
                        result: serde_json::json!({
                            "content": "",
                            "version": 0,
                            "token_count": 0,
                            "message": "No context memory set yet. Use context-memory-update to create one.",
                        }),
                    },
                    Err(e) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": e.to_string(),
                        }),
                    },
                }
            }
            KernelAction::SpawnAgent {
                agent,
                prompt,
                permissions,
                context_messages,
            } => {
                // Build a context slice from the parent task's current context window.
                let slice = self
                    .context_manager
                    .get_slice(
                        &task.id,
                        context_messages as usize,
                        format!("from-parent-{}", task.id),
                    )
                    .await;

                let response = self
                    .cmd_spawn_sub_agent(task.id, &agent, &prompt, &permissions, slice)
                    .await;

                match response {
                    agentos_bus::KernelResponse::SubAgentSpawned { child_task_id } => {
                        KernelActionResult {
                            success: true,
                            result: serde_json::json!({
                                "task_id": child_task_id.to_string(),
                                "agent": agent,
                                "status": "spawned",
                                "message": format!(
                                    "Sub-agent '{}' spawned as task {}. Use await-agents to wait for the result.",
                                    agent, child_task_id
                                ),
                            }),
                        }
                    }
                    agentos_bus::KernelResponse::Error { message } => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": message }),
                    },
                    _ => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": "unexpected response from spawn" }),
                    },
                }
            }
            KernelAction::AwaitAgents { task_ids } => {
                // Parse task IDs and query their current state.
                let mut parsed_ids = Vec::with_capacity(task_ids.len());
                for id_str in &task_ids {
                    match id_str.parse::<agentos_types::TaskID>() {
                        Ok(id) => parsed_ids.push(id),
                        Err(_) => {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": format!("invalid task_id: {}", id_str)
                                }),
                            };
                        }
                    }
                }

                let response = self.cmd_await_sub_agents(task.id, &parsed_ids).await;

                match response {
                    agentos_bus::KernelResponse::SubAgentResults { results } => {
                        let results_json: Vec<serde_json::Value> = results
                            .iter()
                            .map(|(id, summary)| {
                                serde_json::json!({
                                    "task_id": id.to_string(),
                                    "summary": summary,
                                })
                            })
                            .collect();
                        KernelActionResult {
                            success: true,
                            result: serde_json::json!({ "results": results_json }),
                        }
                    }
                    agentos_bus::KernelResponse::Error { message } => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": message }),
                    },
                    _ => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": "unexpected response from await" }),
                    },
                }
            }
            KernelAction::PollAgents {
                task_ids,
                include_progress,
            } => {
                // Cap the number of task IDs to bound per-call scheduler work.
                if task_ids.len() > 50 {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!(
                                "poll_agents supports at most 50 task_ids per call (got {})",
                                task_ids.len()
                            )
                        }),
                    };
                }
                let mut results = Vec::with_capacity(task_ids.len());
                for id_str in &task_ids {
                    match id_str.parse::<agentos_types::TaskID>() {
                        Ok(tid) => {
                            if let Some(child_task) = self.scheduler.get_task(&tid).await {
                                // Verify the caller is the parent
                                if child_task.parent_task_id != Some(task.id) {
                                    results.push(serde_json::json!({
                                        "task_id": id_str,
                                        "error": "not parent of this task"
                                    }));
                                    continue;
                                }
                                let state_label = format!("{:?}", child_task.state);
                                let mut entry = serde_json::json!({
                                    "task_id": id_str,
                                    "state": state_label,
                                    "spawn_depth": child_task.spawn_depth,
                                });
                                if include_progress {
                                    // Include last few history messages as progress
                                    let history_len = child_task.history.len();
                                    let recent: Vec<String> = child_task
                                        .history
                                        .iter()
                                        .rev()
                                        .take(3)
                                        .filter_map(|m| {
                                            m.payload
                                                .data
                                                .get("content")
                                                .and_then(|v| v.as_str())
                                                .map(|content: &str| {
                                                    let truncated: String =
                                                        content.chars().take(200).collect();
                                                    if truncated.len() < content.chars().count() {
                                                        format!("{}...", truncated)
                                                    } else {
                                                        content.to_string()
                                                    }
                                                })
                                        })
                                        .collect();
                                    if let Some(obj) = entry.as_object_mut() {
                                        obj.insert(
                                            "iterations_approx".into(),
                                            serde_json::json!(history_len / 2),
                                        );
                                        obj.insert(
                                            "recent_messages".into(),
                                            serde_json::json!(recent),
                                        );
                                    }
                                }
                                results.push(entry);
                            } else {
                                results.push(serde_json::json!({
                                    "task_id": id_str,
                                    "error": "task not found"
                                }));
                            }
                        }
                        Err(_) => {
                            results.push(serde_json::json!({
                                "task_id": id_str,
                                "error": "invalid task_id"
                            }));
                        }
                    }
                }
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({ "results": results }),
                }
            }
            KernelAction::CancelAgent { task_id, reason } => {
                match task_id.parse::<agentos_types::TaskID>() {
                    Ok(tid) => {
                        // Verify the caller is the parent
                        let is_parent = self
                            .scheduler
                            .get_task(&tid)
                            .await
                            .map(|t| t.parent_task_id == Some(task.id))
                            .unwrap_or(false);
                        if !is_parent {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": "not parent of this task — cannot cancel"
                                }),
                            };
                        }

                        // Cancel the task (cascades to children via existing logic)
                        let response = self.cmd_cancel_task(tid).await;
                        self.audit_log(AuditEntry {
                            timestamp: Utc::now(),
                            trace_id,
                            event_type: AuditEventType::TaskStateChanged,
                            agent_id: Some(task.agent_id),
                            task_id: Some(tid),
                            tool_id: None,
                            details: serde_json::json!({
                                "action": "cancel_agent",
                                "new_state": "cancelled",
                                "reason": reason,
                                "cancelled_by": task.id.to_string(),
                            }),
                            severity: AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        });
                        match response {
                            agentos_bus::KernelResponse::Success { .. } => KernelActionResult {
                                success: true,
                                result: serde_json::json!({
                                    "cancelled": true,
                                    "task_id": task_id,
                                    "reason": reason,
                                }),
                            },
                            agentos_bus::KernelResponse::Error { message } => KernelActionResult {
                                success: false,
                                result: serde_json::json!({ "error": message }),
                            },
                            _ => KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": "unexpected response from cancel"
                                }),
                            },
                        }
                    }
                    Err(_) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("invalid task_id: {}", task_id)
                        }),
                    },
                }
            }
            KernelAction::SpawnAsync {
                target_agent,
                prompt,
                priority,
            } => {
                self.execute_spawn_async(task, &target_agent, &prompt, priority)
                    .await
            }
            KernelAction::A2ADelegate {
                agent_url,
                capability,
                input,
                token,
                wait_for_result,
            } => {
                // SSRF protection: resolve the hostname and reject private/internal addresses.
                if let Some(ssrf_err) = check_a2a_url_ssrf(&agent_url).await {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": ssrf_err,
                            "agent_url": agent_url,
                        }),
                    };
                }

                let mut client = agentos_mcp::a2a::A2AClient::new(&agent_url);
                if let Some(ref t) = token {
                    client = client.with_token(t);
                }

                let sender_url = format!("agentos://agent/{}", task.agent_id);

                match client
                    .submit_task(&capability, input.clone(), &sender_url)
                    .await
                {
                    Ok(task_id) => {
                        if !wait_for_result {
                            KernelActionResult {
                                success: true,
                                result: serde_json::json!({
                                    "task_id": task_id,
                                    "agent_url": agent_url,
                                    "capability": capability,
                                    "status": "submitted",
                                }),
                            }
                        } else {
                            // Poll until terminal with 5-minute timeout
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(300);
                            loop {
                                if std::time::Instant::now() > deadline {
                                    break KernelActionResult {
                                        success: false,
                                        result: serde_json::json!({
                                            "error": "A2A task timed out after 300s",
                                            "task_id": task_id,
                                        }),
                                    };
                                }
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                match client.poll_task(&task_id).await {
                                    Ok(a2a_task) if a2a_task.is_terminal() => {
                                        break KernelActionResult {
                                            success: matches!(
                                                a2a_task.status,
                                                agentos_mcp::a2a::A2ATaskStatus::Completed { .. }
                                            ),
                                            result: serde_json::to_value(&a2a_task)
                                                .unwrap_or(serde_json::json!({})),
                                        };
                                    }
                                    Ok(_) => continue,
                                    Err(e) => {
                                        break KernelActionResult {
                                            success: false,
                                            result: serde_json::json!({
                                                "error": format!("Poll failed: {}", e),
                                                "task_id": task_id,
                                            }),
                                        };
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("A2A delegation failed: {}", e),
                            "agent_url": agent_url,
                            "capability": capability,
                        }),
                    },
                }
            }
            KernelAction::EventSubscribeAction {
                event_filter,
                payload_filter,
                throttle,
                priority,
            } => {
                self.execute_event_subscribe(
                    task,
                    event_filter,
                    payload_filter,
                    throttle,
                    priority,
                    trace_id,
                )
                .await
            }
            KernelAction::EventUnsubscribeAction { subscription_id } => {
                self.execute_event_unsubscribe(task, subscription_id, trace_id)
                    .await
            }
            KernelAction::EventListSubscriptionsAction => {
                self.execute_event_list_subscriptions(task).await
            }
            KernelAction::EventListAvailableAction => self.execute_event_list_available(task).await,
            KernelAction::SetTimer {
                name,
                delay_secs,
                agent_name,
                action,
            } => {
                self.execute_set_timer(task, name, delay_secs, agent_name, action)
                    .await
            }
            KernelAction::CancelTimer { name } => self.execute_cancel_timer(task, name).await,
            KernelAction::ListTimers => self.execute_list_timers(task).await,
            KernelAction::ScheduleOnce {
                name,
                task_prompt,
                agent_name,
                fire_at,
            } => {
                self.execute_schedule_once(task, name, task_prompt, agent_name, fire_at)
                    .await
            }
            KernelAction::CancelOnceJob { name } => self.execute_cancel_once_job(task, name).await,
            KernelAction::ListOnceJobs => self.execute_list_once_jobs(task).await,
            KernelAction::ChannelSend {
                channel,
                text,
                thread_id,
            } => {
                self.execute_channel_send(task, channel, text, thread_id, trace_id)
                    .await
            }
        };

        let severity = if result.success {
            agentos_audit::AuditSeverity::Info
        } else {
            agentos_audit::AuditSeverity::Error
        };

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "kernel_action": action_name,
                "success": result.success,
            }),
            severity,
            reversible: false,
            rollback_ref: None,
        });

        result
    }

    async fn execute_spawn_async(
        &self,
        task: &AgentTask,
        target_agent: &str,
        prompt: &str,
        priority: u8,
    ) -> KernelActionResult {
        let timeout_secs = self.config.kernel.default_task_timeout_secs;
        match self
            .handle_spawn_async(task, target_agent, prompt, priority, timeout_secs)
            .await
        {
            Ok(value) => KernelActionResult {
                success: true,
                result: value,
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_delegate_task(
        &self,
        task: &AgentTask,
        target_agent: &str,
        prompt: &str,
        priority: u8,
    ) -> KernelActionResult {
        let timeout_secs = self.config.kernel.default_task_timeout_secs;
        match self
            .handle_task_delegation(task, target_agent, prompt, priority, timeout_secs)
            .await
        {
            Ok(value) => KernelActionResult {
                success: true,
                result: value,
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_send_message(
        &self,
        task: &AgentTask,
        to: &str,
        content: &str,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let from_name = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_id(&task.agent_id) {
                Some(agent) => agent.name.clone(),
                None => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("Sender agent '{}' not found", task.agent_id)
                        }),
                    };
                }
            }
        };

        let registry = self.agent_registry.read().await;
        let to_agent = match registry.get_by_name(to) {
            Some(a) => a.clone(),
            None => {
                // Fallback: try parsing as UUID for agents that use IDs instead of names
                match to.parse::<AgentID>() {
                    Ok(id) => match registry.get_by_id(&id) {
                        Some(a) => a.clone(),
                        None => {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": format!("Target agent '{}' not found", to)
                                }),
                            };
                        }
                    },
                    Err(_) => {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": format!("Target agent '{}' not found", to)
                            }),
                        };
                    }
                }
            }
        };
        drop(registry);

        let now = chrono::Utc::now();
        let ttl_seconds: u64 = 60;
        let mut msg = AgentMessage {
            id: MessageID::new(),
            from: task.agent_id,
            to: MessageTarget::Direct(to_agent.id),
            content: MessageContent::Text(content.to_string()),
            reply_to: None,
            timestamp: now,
            trace_id,
            signature: None,
            ttl_seconds,
            expires_at: Some(now + chrono::Duration::seconds(ttl_seconds as i64)),
        };

        // Sign the message with the sender's Ed25519 identity key (Spec §7).
        // Return early if signing fails so the unsigned message is never sent
        // (the bus would reject it anyway, but with a misleading error).
        let payload = msg.signing_payload();
        match self
            .identity_manager
            .sign_message(&task.agent_id, &payload)
            .await
        {
            Ok(sig_hex) => msg.signature = Some(sig_hex),
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Agent has no identity key — message signing failed: {}", e)
                    }),
                };
            }
        }

        match self.message_bus.send_direct(msg).await {
            Ok(_) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "status": "delivered",
                    "to": to,
                    "from": from_name,
                }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_escalation(
        &self,
        task: &AgentTask,
        reason: EscalationReason,
        context_summary: &str,
        decision_point: &str,
        options: &[String],
        urgency: &str,
        blocking: bool,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let severity = match urgency {
            "critical" | "high" => agentos_audit::AuditSeverity::Security,
            _ => agentos_audit::AuditSeverity::Warn,
        };

        // Record escalation in audit log
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::PermissionDenied,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "escalation": true,
                "reason": format!("{:?}", reason),
                "context_summary": context_summary,
                "decision_point": decision_point,
                "options": options,
                "urgency": urgency,
                "blocking": blocking,
            }),
            severity,
            reversible: false,
            rollback_ref: None,
        });

        // Store escalation for the escalation manager
        self.escalation_manager
            .create_escalation(
                task.id,
                task.agent_id,
                reason,
                context_summary.to_string(),
                decision_point.to_string(),
                options.to_vec(),
                urgency.to_string(),
                blocking,
                trace_id,
                None, // auto_action: default deny on expiry
            )
            .await;

        // If blocking, set task state to Waiting
        if blocking {
            self.scheduler
                .update_state(&task.id, TaskState::Waiting)
                .await
                .ok();
        }

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "status": if blocking { "escalation_pending_blocking" } else { "escalation_logged" },
                "message": if blocking {
                    "Task paused. Waiting for human review."
                } else {
                    "Escalation logged. Continuing task execution."
                },
                "urgency": urgency,
            }),
        }
    }

    async fn execute_switch_partition(
        &self,
        task: &AgentTask,
        partition: &str,
    ) -> KernelActionResult {
        let target_partition = match partition {
            "scratchpad" => ContextPartition::Scratchpad,
            "active" => ContextPartition::Active,
            _ => ContextPartition::Active,
        };

        match self
            .context_manager
            .set_partition_for_task(&task.id, target_partition)
            .await
        {
            Ok(()) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "status": "partition_switched",
                    "partition": partition,
                }),
            },
            Err(_) => KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "Context not found for task"
                }),
            },
        }
    }

    async fn execute_memory_block_write(
        &self,
        task: &AgentTask,
        label: &str,
        content: &str,
    ) -> KernelActionResult {
        match self.memory_blocks.write(&task.agent_id, label, content) {
            Ok(block) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "status": "memory_block_written",
                    "label": block.label,
                    "size": block.content.len(),
                }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_memory_block_read(&self, task: &AgentTask, label: &str) -> KernelActionResult {
        match self.memory_blocks.get(&task.agent_id, label) {
            Ok(Some(block)) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "label": block.label,
                    "content": block.content,
                    "updated_at": block.updated_at.to_rfc3339(),
                }),
            },
            Ok(None) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": format!("Memory block '{}' not found", label) }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_memory_block_list(&self, task: &AgentTask) -> KernelActionResult {
        match self.memory_blocks.list(&task.agent_id) {
            Ok(blocks) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "count": blocks.len(),
                    "blocks": blocks.into_iter().map(|b| serde_json::json!({
                        "label": b.label,
                        "size": b.content.len(),
                        "updated_at": b.updated_at.to_rfc3339(),
                    })).collect::<Vec<_>>(),
                }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_memory_block_delete(
        &self,
        task: &AgentTask,
        label: &str,
    ) -> KernelActionResult {
        match self.memory_blocks.delete(&task.agent_id, label) {
            Ok(deleted) => KernelActionResult {
                success: deleted,
                result: serde_json::json!({
                    "deleted": deleted,
                    "label": label,
                }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    /// Execute a fire-and-forget notification on behalf of a running task.
    ///
    /// Defense-in-depth: validates `user.notify:w` from the task's capability
    /// token even though `ToolRunner` already checked it.
    async fn execute_notify_user(
        &self,
        task: &AgentTask,
        subject: String,
        body: String,
        priority: String,
        trace_id: TraceID,
    ) -> KernelActionResult {
        // Defense-in-depth permission check.
        if !task
            .capability_token
            .permissions
            .check(agentos_capability::PERM_USER_NOTIFY, PermissionOp::Write)
        {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!(
                        "Permission denied: '{}:w' required for notify-user",
                        agentos_capability::PERM_USER_NOTIFY
                    )
                }),
            };
        }

        let priority_parsed = parse_priority(&priority);

        let agent_name = {
            let reg = self.agent_registry.read().await;
            reg.get_by_id(&task.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string())
        };
        let subject_prefixed = format!("[{agent_name}] {}", subject.as_str());
        let subject_line: String = subject_prefixed.chars().take(80).collect();

        let msg = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Agent(task.agent_id),
            task_id: Some(task.id),
            trace_id,
            kind: UserMessageKind::Notification,
            priority: priority_parsed,
            subject: subject_line,
            body,
            interaction: None,
            delivery_status: HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: Some(task.id.to_string()),
            reply_to_external_id: None,
        };

        let notification_id = msg.id;

        match self.notification_router.deliver(msg).await {
            Ok(_) => {
                self.audit_log(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: AuditEventType::NotificationSent,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "notification_id": notification_id.to_string(),
                        "source": "notify-user tool",
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "status": "notification_sent",
                        "notification_id": notification_id.to_string(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    /// Execute a blocking ask-user interaction.
    ///
    /// Delivers a `Question` notification to the user inbox, then parks the
    /// task in `Waiting` state until the user responds (or the timeout fires).
    ///
    /// Defense-in-depth: validates `user.interact:x` from the task's capability
    /// token even though `ToolRunner` already checked it.
    ///
    /// While parked the tokio runtime continues executing other tasks — this is
    /// a cooperative async suspension, not a thread block.
    #[allow(clippy::too_many_arguments)]
    async fn execute_ask_user(
        &self,
        task: &AgentTask,
        question: String,
        options: Option<Vec<String>>,
        timeout_secs: u64,
        priority: String,
        auto_action: String,
        trace_id: TraceID,
    ) -> KernelActionResult {
        // Defense-in-depth permission check.
        if !task.capability_token.permissions.check(
            agentos_capability::PERM_USER_INTERACT,
            PermissionOp::Execute,
        ) {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!(
                        "Permission denied: '{}:x' required for ask-user",
                        agentos_capability::PERM_USER_INTERACT
                    )
                }),
            };
        }

        let priority_parsed = parse_priority(&priority);
        // Clamp to the range declared in the TOML manifest (10 s – 24 h).
        let timeout_secs = timeout_secs.clamp(10, 86_400);
        let expires_at = Utc::now() + chrono::Duration::seconds(timeout_secs as i64);

        let agent_name = {
            let reg = self.agent_registry.read().await;
            reg.get_by_id(&task.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string())
        };
        let subject_prefixed = format!("[{agent_name}] {}", question.as_str());
        let subject_line: String = subject_prefixed.chars().take(80).collect();
        let body_prefixed = format!("{agent_name} asks:\n\n{question}");

        let msg = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Agent(task.agent_id),
            task_id: Some(task.id),
            trace_id,
            kind: UserMessageKind::Question {
                question: question.clone(),
                options,
                free_text_allowed: true,
            },
            priority: priority_parsed,
            subject: subject_line,
            body: body_prefixed,
            interaction: Some(InteractionRequest {
                blocking: true,
                timeout_secs,
                auto_action: auto_action.clone(),
                // max_concurrent enforcement is deferred; use the default of 3.
                max_concurrent: 3,
            }),
            delivery_status: HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: Some(expires_at),
            read: false,
            thread_id: Some(task.id.to_string()),
            reply_to_external_id: None,
        };

        let notification_id = msg.id;

        // Deliver and obtain the blocking receiver.
        let rx = match self.notification_router.deliver(msg).await {
            Ok(Some(rx)) => rx,
            Ok(None) => {
                // Should not happen since blocking=true always produces a receiver.
                tracing::error!(
                    task_id = %task.id,
                    "ask-user: blocking delivery returned no receiver"
                );
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": "Internal error: blocking notification returned no receiver"
                    }),
                };
            }
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": e.to_string() }),
                };
            }
        };

        // Set task to Waiting so the running_count drops and new tasks can be
        // scheduled while this one is parked.
        if let Err(e) = self
            .scheduler
            .update_state(&task.id, TaskState::Waiting)
            .await
        {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "ask-user: failed to set task state to Waiting"
            );
        }

        tracing::info!(
            task_id = %task.id,
            notification_id = %notification_id,
            timeout_secs,
            "ask-user: task parked, awaiting user response"
        );

        // Await user response with a generous safety timeout (sweep fires at most
        // 10 minutes after expiry, so we add a 600 s buffer above timeout_secs).
        let safety_timeout = Duration::from_secs(timeout_secs.saturating_add(600));
        let response = tokio::select! {
            result = tokio::time::timeout(safety_timeout, rx) => {
                match result {
                    Ok(Ok(resp)) => resp,
                    Ok(Err(_recv_err)) => {
                        // Oneshot sender was dropped (sweep fired and removed the entry).
                        // Clean up in case there is a stale entry still in the map.
                        self.notification_router
                            .remove_waiting_task(&notification_id)
                            .await;
                        UserResponse {
                            text: auto_action.clone(),
                            responded_at: Utc::now(),
                            channel: DeliveryChannel::cli(),
                        }
                    }
                    Err(_timeout) => {
                        // Safety timeout fired before sweep — remove the dead sender now
                        // so it doesn't linger in waiting_tasks until the next sweep cycle.
                        self.notification_router
                            .remove_waiting_task(&notification_id)
                            .await;
                        tracing::warn!(
                            task_id = %task.id,
                            notification_id = %notification_id,
                            "ask-user: safety timeout fired; returning auto_action"
                        );
                        UserResponse {
                            text: auto_action.clone(),
                            responded_at: Utc::now(),
                            channel: DeliveryChannel::cli(),
                        }
                    }
                }
            }
            _ = self.cancellation_token.cancelled() => {
                // Kernel shutting down — clean up the dead sender.
                self.notification_router
                    .remove_waiting_task(&notification_id)
                    .await;
                tracing::info!(
                    task_id = %task.id,
                    "ask-user: kernel shutting down while waiting for user response"
                );
                UserResponse {
                    text: "kernel_shutdown".to_string(),
                    responded_at: Utc::now(),
                    channel: DeliveryChannel::cli(),
                }
            }
        };

        // Restore task to Running — only if it hasn't been cancelled/failed while
        // parked. A terminal state (Cancelled/Failed/Complete) means the operator
        // explicitly stopped the task; we must not override that decision.
        let restored = self
            .scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Running)
            .await
            .unwrap_or(false);

        if !restored {
            tracing::info!(
                task_id = %task.id,
                "ask-user: task entered terminal state while waiting for user response; aborting"
            );
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "Task was cancelled or terminated while waiting for user response"
                }),
            };
        }

        // Audit the received response.
        self.audit_log(AuditEntry {
            timestamp: Utc::now(),
            trace_id,
            event_type: AuditEventType::UserResponseReceived,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "notification_id": notification_id.to_string(),
                "channel": response.channel.to_string(),
                "auto_actioned": response.text == auto_action || response.text == "kernel_shutdown",
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "response": response.text,
                "channel": response.channel.to_string(),
                "responded_at": response.responded_at.to_rfc3339(),
            }),
        }
    }

    /// Execute a synchronous agent-to-agent RPC call.
    ///
    /// Creates a child task for the target agent, registers a pending call
    /// in `RpcManager`, then blocks until the child completes. The child
    /// task runs through the same `execute_task_sync` path as any other
    /// task, preserving all security and audit guarantees.
    async fn execute_agent_rpc_call(
        &self,
        task: &AgentTask,
        target_agent: &str,
        prompt: &str,
        timeout_secs: u64,
        trace_id: TraceID,
    ) -> KernelActionResult {
        // 1. Resolve target agent
        let registry = self.agent_registry.read().await;
        let target = match registry.get_by_name(target_agent) {
            Some(a) if a.status != AgentStatus::Offline => a.clone(),
            Some(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Agent '{}' is offline", target_agent)
                    }),
                };
            }
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Agent '{}' not found", target_agent)
                    }),
                };
            }
        };
        let target_permissions = registry.compute_effective_permissions(&target.id);
        drop(registry);

        // 1b. Prevent self-calls — an agent cannot RPC itself
        if target.id == task.agent_id {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "An agent cannot call itself via RPC"
                }),
            };
        }

        // 2. Compute child permissions (intersection of parent + target)
        let child_permissions = task.capability_token.permissions.clone();
        let mut effective_permissions = child_permissions.intersect(&target_permissions);
        if task.autonomous {
            effective_permissions.grant_op("process.exec".to_string(), PermissionOp::Execute, None);
        }

        // 3. Issue capability token for child task
        let child_task_id = TaskID::new();
        let child_token = match self.capability_engine.issue_token(
            child_task_id,
            target.id,
            task.capability_token.allowed_tools.clone(),
            task.capability_token.allowed_intents.clone(),
            effective_permissions,
            Duration::from_secs(timeout_secs),
        ) {
            Ok(token) => token,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Failed to issue capability token: {}", e)
                    }),
                };
            }
        };

        // 4. Register the RPC call in the manager (get oneshot receiver)
        let rx = match self
            .rpc_manager
            .register_call(task.id, target.id, child_task_id, timeout_secs)
            .await
        {
            Ok(rx) => rx,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": e.to_string() }),
                };
            }
        };

        // 5. Create and register the child task
        let child_task = AgentTask {
            id: child_task_id,
            state: TaskState::Queued,
            agent_id: target.id,
            capability_token: child_token,
            assigned_llm: None,
            priority: task.priority,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(timeout_secs),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(task.id),
            reasoning_hints: Some(crate::commands::task::infer_reasoning_hints(prompt)),
            max_iterations: None,
            trigger_source: None,
            autonomous: task.autonomous,
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            spawner_agent_id: None,
        };

        self.scheduler.register_external(child_task.clone()).await;
        self.scheduler
            .update_state_if_not_terminal(&child_task_id, TaskState::Running)
            .await
            .ok();
        self.scheduler.mark_started(&child_task_id).await.ok();

        // 6. Emit audit and event
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::TaskCreated,
            agent_id: Some(target.id),
            task_id: Some(child_task_id),
            tool_id: None,
            details: serde_json::json!({
                "rpc_call": true,
                "caller_task_id": task.id.to_string(),
                "caller_agent_id": task.agent_id.to_string(),
                "target_agent": target_agent,
                "timeout_secs": timeout_secs,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        self.emit_event_with_trace(
            EventType::AgentRpcCallStarted,
            EventSource::AgentMessageBus,
            EventSeverity::Info,
            serde_json::json!({
                "caller_task_id": task.id.to_string(),
                "caller_agent_id": task.agent_id.to_string(),
                "rpc_task_id": child_task_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent,
                "timeout_secs": timeout_secs,
            }),
            0,
            Some(trace_id),
            Some(task.agent_id),
            Some(task.id),
        )
        .await;

        // 7. Set caller task to Waiting while the RPC child runs
        self.scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Waiting)
            .await
            .ok();

        // 8. Start trace for child task
        self.trace_collector
            .start_task(child_task_id, target.id, prompt)
            .await;

        // 9. Execute child task. Box::pin breaks the recursive async future
        //    cycle (execute_agent_rpc_call → execute_task_sync → tool loop →
        //    dispatch_kernel_action → execute_agent_rpc_call).
        let child_trace_id = TraceID::new();
        let start = chrono::Utc::now();
        let child_task_span = self.otel.start_task_span(
            &child_task.id.to_string(),
            &child_task.agent_id.to_string(),
            &target.model,
        );
        self.otel.adjust_active_tasks(1);
        let child_result =
            Box::pin(self.execute_task_sync(&child_task, &child_trace_id, &child_task_span)).await;
        let duration_ms = (chrono::Utc::now() - start).num_milliseconds().max(0) as u64;

        // 10. Finish child trace and handle completion
        match child_result {
            Ok(task_result) => {
                self.trace_collector
                    .finish_task(&child_task_id, "Complete", chrono::Utc::now())
                    .await;
                child_task_span.set_string_attribute("task.status", "complete");
                child_task_span.set_i64_attribute("task.iterations", task_result.iterations as i64);
                self.otel.record_task_metric(
                    &child_task.agent_id.to_string(),
                    "complete",
                    duration_ms,
                );
                self.otel.adjust_active_tasks(-1);
                self.complete_task_success(&child_task, &task_result, duration_ms, child_trace_id)
                    .await;
            }
            Err(e) => {
                self.trace_collector
                    .finish_task(&child_task_id, "Failed", chrono::Utc::now())
                    .await;
                child_task_span.set_string_attribute("task.status", "failed");
                child_task_span.record_error(e.to_string());
                self.otel.record_task_metric(
                    &child_task.agent_id.to_string(),
                    "failed",
                    duration_ms,
                );
                self.otel.adjust_active_tasks(-1);
                self.complete_task_failure(&child_task, e, duration_ms, child_trace_id)
                    .await;
            }
        }

        // 11. Restore caller task to Running
        self.scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Running)
            .await
            .ok();

        // 12. Wait for the result from the oneshot (should already be available
        // since complete_task_success/failure calls rpc_manager.complete_call)
        let safety_timeout = Duration::from_secs(timeout_secs.saturating_add(30));
        let rpc_result = tokio::select! {
            result = tokio::time::timeout(safety_timeout, rx) => {
                match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(_)) => {
                        // Sender dropped — RPC was never completed (should not happen)
                        crate::rpc_manager::RpcResult {
                            output: String::new(),
                            success: false,
                            error: Some("RPC call aborted: result channel dropped".to_string()),
                        }
                    }
                    Err(_) => {
                        // Safety timeout
                        crate::rpc_manager::RpcResult {
                            output: String::new(),
                            success: false,
                            error: Some("RPC call timed out".to_string()),
                        }
                    }
                }
            }
            _ = self.cancellation_token.cancelled() => {
                crate::rpc_manager::RpcResult {
                    output: String::new(),
                    success: false,
                    error: Some("Kernel shutting down".to_string()),
                }
            }
        };

        // 13. Emit completion event
        self.emit_event_with_trace(
            EventType::AgentRpcCallCompleted,
            EventSource::AgentMessageBus,
            if rpc_result.success {
                EventSeverity::Info
            } else {
                EventSeverity::Warning
            },
            serde_json::json!({
                "caller_task_id": task.id.to_string(),
                "rpc_task_id": child_task_id.to_string(),
                "success": rpc_result.success,
                "error": rpc_result.error,
            }),
            0,
            Some(trace_id),
            Some(task.agent_id),
            Some(task.id),
        )
        .await;

        if rpc_result.success {
            KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "status": "rpc_complete",
                    "target_agent": target_agent,
                    "rpc_task_id": child_task_id.to_string(),
                    "output": rpc_result.output,
                }),
            }
        } else {
            KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": rpc_result.error.unwrap_or_else(|| "RPC call failed".to_string()),
                    "rpc_task_id": child_task_id.to_string(),
                }),
            }
        }
    }

    // ─── Event self-subscription handlers ────────────────────────────
    //
    // These power the four `event-*` agent tools. Each handler runs with
    // `task.agent_id` as the calling identity and never accepts a
    // `target_agent` argument — agents can only manage their own
    // subscriptions. Per-category permission gating happens in
    // `event_permissions::check_subscribe_permission`.

    async fn execute_event_subscribe(
        &self,
        task: &AgentTask,
        event_filter: String,
        payload_filter: Option<String>,
        throttle: Option<String>,
        priority: Option<String>,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let parsed_filter = match crate::event_bus::parse_event_type_filter(&event_filter) {
            Some(f) => f,
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!(
                            "Invalid event filter '{}'. Use 'all', 'category:<Name>', or an exact event type like 'AgentAdded'.",
                            event_filter
                        ),
                    }),
                };
            }
        };

        // Permission check — gated per category. Uses the capability token
        // permissions, which already include any role-derived observe grants.
        if let Err(e) = crate::event_permissions::check_subscribe_permission(
            &task.capability_token.permissions,
            &parsed_filter,
        ) {
            self.audit_log(AuditEntry {
                timestamp: Utc::now(),
                trace_id,
                event_type: AuditEventType::PermissionDenied,
                agent_id: Some(task.agent_id),
                task_id: Some(task.id),
                tool_id: None,
                details: serde_json::json!({
                    "tool": "event-subscribe",
                    "event_filter": event_filter,
                    "reason": e.to_string(),
                }),
                severity: AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            });
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": e.to_string(),
                    "hint": "Ask an operator to grant the required `events.<category>:observe` permission, then retry.",
                }),
            };
        }

        let throttle_policy = match throttle.as_deref() {
            None | Some("") | Some("none") => ThrottlePolicy::None,
            Some(s) => match parse_throttle_str(s) {
                Some(p) => p,
                None => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!(
                                "Invalid throttle '{}'. Use 'none', 'once_per:<duration>' (e.g. 'once_per:30s'), or 'max:<count>/<duration>' (e.g. 'max:5/60s').",
                                s
                            ),
                        }),
                    };
                }
            },
        };

        let sub_priority = match crate::event_bus::parse_subscription_priority(priority.as_deref())
        {
            Some(p) => p,
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!(
                            "Invalid priority '{}'. Use 'critical', 'high', 'normal', or 'low'.",
                            priority.as_deref().unwrap_or_default()
                        ),
                    }),
                };
            }
        };

        let payload_filter = payload_filter.and_then(|raw| {
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
            event_type_filter: parsed_filter,
            filter: payload_filter.clone(),
            priority: sub_priority,
            throttle: throttle_policy,
            enabled: true,
            created_at: Utc::now(),
        };

        let sub_id = self.event_bus.subscribe(sub).await;

        self.audit_log(AuditEntry {
            timestamp: Utc::now(),
            trace_id,
            event_type: AuditEventType::EventSubscriptionCreated,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "subscription_id": sub_id.to_string(),
                "event_filter": event_filter,
                "payload_filter": payload_filter,
                "self_subscribed": true,
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "subscription_id": sub_id.to_string(),
                "event_filter": event_filter,
                "status": "subscribed",
                "message": "Subscription created. The kernel will dispatch matching events as new tasks for this agent.",
            }),
        }
    }

    async fn execute_event_unsubscribe(
        &self,
        task: &AgentTask,
        subscription_id: String,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let id = match subscription_id.parse::<SubscriptionID>() {
            Ok(id) => id,
            Err(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Invalid subscription ID: {}", subscription_id),
                    }),
                };
            }
        };

        // Verify the subscription belongs to the calling agent — agents must
        // not be able to cancel subscriptions owned by other agents.
        match self.event_bus.get_subscription(&id).await {
            Some(sub) if sub.agent_id == task.agent_id => {}
            Some(_) => {
                self.audit_log(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: AuditEventType::PermissionDenied,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "tool": "event-unsubscribe",
                        "subscription_id": subscription_id,
                        "reason": "subscription belongs to a different agent",
                    }),
                    severity: AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": "Subscription belongs to a different agent",
                    }),
                };
            }
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Subscription '{}' not found", subscription_id),
                    }),
                };
            }
        }

        if self.event_bus.unsubscribe(&id).await {
            self.audit_log(AuditEntry {
                timestamp: Utc::now(),
                trace_id,
                event_type: AuditEventType::EventSubscriptionRemoved,
                agent_id: Some(task.agent_id),
                task_id: Some(task.id),
                tool_id: None,
                details: serde_json::json!({
                    "subscription_id": subscription_id,
                    "self_unsubscribed": true,
                }),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            });
            KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "subscription_id": subscription_id,
                    "status": "unsubscribed",
                }),
            }
        } else {
            KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!("Subscription '{}' not found", subscription_id),
                }),
            }
        }
    }

    async fn execute_event_list_subscriptions(&self, task: &AgentTask) -> KernelActionResult {
        let subs = self
            .event_bus
            .list_subscriptions_for_agent(&task.agent_id)
            .await;
        let values: Vec<serde_json::Value> = subs
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id.to_string(),
                    "event_type_filter": format!("{:?}", s.event_type_filter),
                    "payload_filter": s.filter,
                    "priority": format!("{:?}", s.priority),
                    "throttle": format!("{:?}", s.throttle),
                    "enabled": s.enabled,
                    "created_at": s.created_at.to_rfc3339(),
                })
            })
            .collect();
        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "count": values.len(),
                "subscriptions": values,
            }),
        }
    }

    async fn execute_event_list_available(&self, task: &AgentTask) -> KernelActionResult {
        // Static category → event-types catalog. Mirrors EventType::category()
        // and stays in sync because the kernel test suite asserts coverage.
        let category_events: &[(&str, &str, &[&str])] = &[
            (
                "AgentLifecycle",
                "events.agent_lifecycle",
                &[
                    "AgentAdded",
                    "AgentRemoved",
                    "AgentPermissionGranted",
                    "AgentPermissionRevoked",
                ],
            ),
            (
                "TaskLifecycle",
                "events.task_lifecycle",
                &[
                    "TaskStarted",
                    "TaskCompleted",
                    "TaskFailed",
                    "TaskTimedOut",
                    "TaskSuspended",
                    "TaskDelegated",
                    "TaskRetrying",
                    "TaskDeadlockDetected",
                    "TaskPreempted",
                ],
            ),
            (
                "SecurityEvents",
                "events.security",
                &[
                    "PromptInjectionAttempt",
                    "CapabilityViolation",
                    "UnauthorizedToolAccess",
                    "SecretsAccessAttempt",
                    "SandboxEscapeAttempt",
                    "AuditLogTamperAttempt",
                    "AgentImpersonationAttempt",
                    "UnverifiedToolInstalled",
                ],
            ),
            (
                "MemoryEvents",
                "events.memory",
                &[
                    "ContextWindowNearLimit",
                    "ContextWindowExhausted",
                    "EpisodicMemoryWritten",
                    "SemanticMemoryConflict",
                    "MemorySearchFailed",
                    "WorkingMemoryEviction",
                ],
            ),
            (
                "SystemHealth",
                "events.system_health",
                &[
                    "CPUSpikeDetected",
                    "MemoryPressure",
                    "DiskSpaceLow",
                    "DiskSpaceCritical",
                    "ProcessCrashed",
                    "NetworkInterfaceDown",
                    "ContainerResourceQuotaExceeded",
                    "KernelSubsystemError",
                    "BudgetWarning",
                    "BudgetExhausted",
                ],
            ),
            (
                "HardwareEvents",
                "events.hardware",
                &[
                    "GPUAvailable",
                    "GPUMemoryPressure",
                    "SensorReadingThresholdExceeded",
                    "DeviceConnected",
                    "DeviceDisconnected",
                    "HardwareAccessGranted",
                    "DeviceMounted",
                    "DeviceUnmounted",
                    "DeviceEjected",
                    "PrintJobSubmitted",
                    "PrintJobCancelled",
                    "AudioCaptureStarted",
                    "AudioCaptureStopped",
                    "AudioPlaybackStarted",
                    "WebcamCaptureStarted",
                    "WebcamCaptureStopped",
                    "BluetoothScanStarted",
                    "BluetoothPairRequested",
                    "BluetoothConnected",
                    "DisplayConfigApplied",
                    "DisplayConfigReverted",
                    "RawUsbDeviceOpened",
                    "RawUsbTransferCompleted",
                ],
            ),
            (
                "ToolEvents",
                "events.tool",
                &[
                    "ToolInstalled",
                    "ToolRemoved",
                    "ToolExecutionFailed",
                    "ToolSandboxViolation",
                    "ToolResourceQuotaExceeded",
                    "ToolChecksumMismatch",
                    "ToolRegistryUpdated",
                    "ToolCallStarted",
                    "ToolCallCompleted",
                    "ToolFallbackAttempted",
                    "ToolFallbackSucceeded",
                    "ToolFallbackExhausted",
                ],
            ),
            (
                "AgentCommunication",
                "events.agent_communication",
                &[
                    "DirectMessageReceived",
                    "BroadcastReceived",
                    "DelegationReceived",
                    "DelegationResponseReceived",
                    "MessageDeliveryFailed",
                    "AgentUnreachable",
                    "AgentRpcCallStarted",
                    "AgentRpcCallCompleted",
                    "AgentRpcCallTimedOut",
                    "SubAgentProgress",
                    "SubAgentCompleted",
                    "SubAgentFailed",
                ],
            ),
            (
                "ScheduleEvents",
                "events.schedule",
                &[
                    "CronJobFired",
                    "ScheduledTaskMissed",
                    "ScheduledTaskCompleted",
                    "ScheduledTaskFailed",
                ],
            ),
            (
                "ExternalEvents",
                "events.external",
                &[
                    "WebhookReceived",
                    "ExternalFileChanged",
                    "ExternalAPIEvent",
                    "ExternalAlertReceived",
                ],
            ),
        ];

        let perms = &task.capability_token.permissions;
        let categories: Vec<serde_json::Value> = category_events
            .iter()
            .map(|(cat_name, perm_resource, events)| {
                let allowed = perms.check(perm_resource, agentos_types::PermissionOp::Observe);
                serde_json::json!({
                    "category": cat_name,
                    "permission": format!("{}:observe", perm_resource),
                    "subscribable": allowed,
                    "events": events,
                })
            })
            .collect();

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "categories": categories,
                "filter_syntax": {
                    "all": "Subscribe to every event (requires observe on every category — usually root-only).",
                    "category": "category:<CategoryName> — e.g. 'category:HardwareEvents'",
                    "exact": "<EventType> — e.g. 'DeviceConnected', or fully qualified 'HardwareEvents.DeviceConnected'",
                },
                "throttle_syntax": {
                    "none": "No throttle (default).",
                    "once_per": "once_per:<duration>  e.g. once_per:30s, once_per:5m",
                    "max": "max:<count>/<duration>  e.g. max:5/60s",
                },
                "priority_values": ["critical", "high", "normal", "low"],
                "tip": "Subscribable=false means you don't have observe permission for that category — ask an operator to grant `events.<category>:observe`.",
            }),
        }
    }
}

/// Parse a throttle string like "once_per:30s" or "max:5/60s".
/// Mirrors the parser in `commands/event.rs` so the agent-tool path does
/// not depend on a private CLI helper.
fn parse_throttle_str(s: &str) -> Option<ThrottlePolicy> {
    if let Some(dur_str) = s.strip_prefix("once_per:") {
        let duration = parse_duration_str(dur_str)?;
        return Some(ThrottlePolicy::MaxOncePerDuration(duration));
    }
    if let Some(rest) = s.strip_prefix("max:") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() != 2 {
            return None;
        }
        let count: u32 = parts[0].parse().ok()?;
        let duration = parse_duration_str(parts[1])?;
        return Some(ThrottlePolicy::MaxCountPerDuration(count, duration));
    }
    None
}

fn parse_duration_str(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        return secs.parse::<u64>().ok().map(std::time::Duration::from_secs);
    }
    if let Some(mins) = s.strip_suffix('m') {
        return mins
            .parse::<u64>()
            .ok()
            .map(|n| std::time::Duration::from_secs(n * 60));
    }
    if let Some(hours) = s.strip_suffix('h') {
        return hours
            .parse::<u64>()
            .ok()
            .map(|n| std::time::Duration::from_secs(n * 3600));
    }
    s.parse::<u64>().ok().map(std::time::Duration::from_secs)
}

/// SSRF protection for outbound A2A delegation requests.
///
/// Parses the URL, resolves the hostname, and checks every resolved IP against
/// private/internal ranges. Returns `Some(error_message)` if the URL should be
/// blocked, `None` if it is safe to proceed.
async fn check_a2a_url_ssrf(url: &str) -> Option<String> {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(e) => return Some(format!("Invalid agent_url: {}", e)),
    };

    // Only allow http/https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Some(format!(
                "Blocked scheme '{}' — only http/https allowed",
                other
            ))
        }
    }

    let host = match parsed.host_str() {
        Some(h) => h.to_string(),
        None => return Some("agent_url has no host".to_string()),
    };

    let port = parsed.port_or_known_default().unwrap_or(80);

    // Resolve and check each IP
    let addrs: Vec<std::net::IpAddr> =
        match tokio::net::lookup_host(format!("{}:{}", host, port)).await {
            Ok(iter) => iter.map(|sa| sa.ip()).collect(),
            Err(e) => return Some(format!("DNS resolution failed for '{}': {}", host, e)),
        };

    for ip in &addrs {
        if is_private_addr(ip) {
            tracing::warn!(
                url = %url,
                %ip,
                "A2A delegation SSRF blocked: private/internal IP"
            );
            return Some(format!(
                "SSRF blocked: '{}' resolves to private/internal IP {}",
                host, ip
            ));
        }
    }

    None
}

/// Returns true if `ip` is a private, loopback, link-local, or otherwise
/// internal address that should never be reachable via agent-initiated A2A.
fn is_private_addr(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                // 100.64.0.0/10 — Carrier-Grade NAT (RFC 6598)
                || {
                    let o = v4.octets();
                    o[0] == 100 && o[1] >= 64 && o[1] < 128
                }
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_addr(&std::net::IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// Parse a priority string into a `NotificationPriority`.
///
/// Unrecognised values fall back to `Info`.
fn parse_priority(s: &str) -> NotificationPriority {
    match s.to_ascii_lowercase().as_str() {
        "warning" => NotificationPriority::Warning,
        "urgent" => NotificationPriority::Urgent,
        "critical" => NotificationPriority::Critical,
        _ => NotificationPriority::Info,
    }
}

impl Kernel {
    async fn execute_set_timer(
        &self,
        task: &AgentTask,
        name: String,
        delay_secs: u64,
        agent_name: String,
        action: TimerAction,
    ) -> KernelActionResult {
        // Resolve agent_name: if it looks like an AgentID, map to the display name.
        let resolved = if let Ok(aid) = agent_name.parse::<AgentID>() {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&aid)
                .map(|a| a.name.clone())
                .unwrap_or(agent_name)
        } else if agent_name.is_empty() {
            // Default to the calling agent's name.
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&task.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string())
        } else {
            agent_name
        };

        match self
            .schedule_manager
            .create_timer(name.clone(), delay_secs, resolved, action, None)
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TimerCreated,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "timer_name": name,
                        "timer_id": id.to_string(),
                        "delay_secs": delay_secs,
                        "source": "agent_tool",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                let fire_at = Utc::now() + Duration::from_secs(delay_secs);
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "timer_id": id.to_string(),
                        "timer_name": name,
                        "fires_at": fire_at.to_rfc3339(),
                        "delay_secs": delay_secs,
                        "message": format!("Timer '{}' set — fires in {}s", name, delay_secs),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_cancel_timer(&self, task: &AgentTask, name: String) -> KernelActionResult {
        match self.schedule_manager.cancel_timer_by_name(&name).await {
            Ok(timer) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TimerCancelled,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({ "timer_name": timer.name, "timer_id": timer.id.to_string() }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "cancelled": true,
                        "timer_name": timer.name,
                        "timer_id": timer.id.to_string(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_list_timers(&self, _task: &AgentTask) -> KernelActionResult {
        let timers = self.schedule_manager.list_timers().await;
        let list: Vec<serde_json::Value> = timers
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id.to_string(),
                    "name": t.name,
                    "agent_name": t.agent_name,
                    "fires_at": t.fire_at.to_rfc3339(),
                })
            })
            .collect();
        KernelActionResult {
            success: true,
            result: serde_json::json!({ "timers": list, "count": list.len() }),
        }
    }

    async fn execute_schedule_once(
        &self,
        task: &AgentTask,
        name: String,
        task_prompt: String,
        agent_name: String,
        fire_at: chrono::DateTime<chrono::Utc>,
    ) -> KernelActionResult {
        let resolved = if let Ok(aid) = agent_name.parse::<AgentID>() {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&aid)
                .map(|a| a.name.clone())
                .unwrap_or(agent_name)
        } else if agent_name.is_empty() {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&task.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string())
        } else {
            agent_name
        };

        match self
            .schedule_manager
            .create_once_job(name.clone(), fire_at, resolved, task_prompt)
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ScheduledJobCreated,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "job_name": name,
                        "schedule_id": id.to_string(),
                        "fire_at": fire_at.to_rfc3339(),
                        "once": true,
                        "source": "agent_tool",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "job_id": id.to_string(),
                        "job_name": name,
                        "fires_at": fire_at.to_rfc3339(),
                        "message": format!("Once-job '{}' scheduled for {}", name, fire_at.to_rfc3339()),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_cancel_once_job(&self, task: &AgentTask, name: String) -> KernelActionResult {
        match self.schedule_manager.cancel_once_job_by_name(&name).await {
            Ok(job) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ScheduledJobDeleted,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({ "job_name": job.name, "job_id": job.id.to_string(), "once": true }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "cancelled": true,
                        "job_name": job.name,
                        "job_id": job.id.to_string(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_list_once_jobs(&self, _task: &AgentTask) -> KernelActionResult {
        let jobs = self.schedule_manager.list_once_jobs().await;
        let list: Vec<serde_json::Value> = jobs
            .iter()
            .map(|j| {
                serde_json::json!({
                    "id": j.id.to_string(),
                    "name": j.name,
                    "agent_name": j.agent_name,
                    "fires_at": j.fire_at.to_rfc3339(),
                })
            })
            .collect();
        KernelActionResult {
            success: true,
            result: serde_json::json!({ "jobs": list, "count": list.len() }),
        }
    }

    /// Send a message to a single connected channel by display name or ID.
    ///
    /// Resolution: try as `ChannelInstanceID` UUID first; fall back to a unique
    /// `display_name` match. Ambiguous display_name (multiple matches) returns
    /// an error listing IDs so the agent can disambiguate.
    ///
    /// Dispatch: Telegram/Ntfy/Email are owned by `notification_router`;
    /// Discord/Slack/WhatsApp/Webhook are owned by `channel_manager`. The
    /// kind drives which transport handles the send. On miss, the error
    /// payload includes `available_channels` so the agent can self-correct
    /// in one shot.
    async fn execute_channel_send(
        &self,
        task: &AgentTask,
        channel: String,
        text: String,
        thread_id: Option<String>,
        trace_id: TraceID,
    ) -> KernelActionResult {
        use agentos_types::ChannelKind;

        // Defense-in-depth permission check.
        if !task
            .capability_token
            .permissions
            .check(agentos_capability::PERM_CHANNEL_SEND, PermissionOp::Write)
        {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!(
                        "Permission denied: '{}:w' required for channel-send",
                        agentos_capability::PERM_CHANNEL_SEND
                    )
                }),
            };
        }

        // Defense-in-depth payload validation (mirrors ChannelSendTool::execute).
        if channel.trim().is_empty() {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "channel-send 'channel' must be non-empty"
                }),
            };
        }
        if text.is_empty() {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "channel-send 'text' must be non-empty"
                }),
            };
        }

        let registered = match self.channel_registry.list_active().await {
            Ok(list) => list,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Failed to list channels: {e}")
                    }),
                };
            }
        };

        let active_summary: Vec<serde_json::Value> = registered
            .iter()
            .filter(|c| c.active)
            .map(|c| {
                serde_json::json!({
                    "id": c.id.to_string(),
                    "name": c.display_name,
                    "kind": c.kind.to_string(),
                })
            })
            .collect();

        // Lookup priority: try UUID first, then display_name. Ambiguous
        // display_name (matches > 1) is an error so the agent can pick by ID.
        let by_id = registered.iter().find(|c| c.id.to_string() == channel);
        let target = if let Some(c) = by_id {
            c
        } else {
            let by_name: Vec<&agentos_types::RegisteredChannel> = registered
                .iter()
                .filter(|c| c.display_name == channel)
                .collect();
            match by_name.len() {
                0 => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("Channel '{channel}' not found"),
                            "available_channels": active_summary,
                        }),
                    };
                }
                1 => by_name[0],
                _ => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!(
                                "Channel name '{channel}' is ambiguous ({} matches). Pass the channel ID instead.",
                                by_name.len()
                            ),
                            "matches": by_name
                                .iter()
                                .map(|c| serde_json::json!({
                                    "id": c.id.to_string(),
                                    "name": c.display_name,
                                    "kind": c.kind.to_string(),
                                }))
                                .collect::<Vec<_>>(),
                        }),
                    };
                }
            }
        };

        if !target.active {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!("Channel '{channel}' is registered but not active"),
                    "available_channels": active_summary,
                }),
            };
        }

        let target_id = target.id;
        let target_name = target.display_name.clone();
        let target_kind = target.kind.clone();

        // Dispatch by kind. Notification-router-owned kinds wrap the text in a
        // UserMessage and route via deliver_to_channel (single-target). The
        // remaining kinds use the channel_manager outbound path.
        let send_result: Result<(), AgentOSError> = match &target_kind {
            ChannelKind::Telegram | ChannelKind::Ntfy | ChannelKind::Email => {
                let agent_name = {
                    let reg = self.agent_registry.read().await;
                    reg.get_by_id(&task.agent_id)
                        .map(|a| a.name.clone())
                        .unwrap_or_else(|| task.agent_id.to_string())
                };
                let subject_line: String = format!("[{agent_name}]").chars().take(80).collect();
                let msg = UserMessage {
                    id: NotificationID::new(),
                    from: NotificationSource::Agent(task.agent_id),
                    task_id: Some(task.id),
                    trace_id,
                    kind: UserMessageKind::Notification,
                    priority: NotificationPriority::Info,
                    subject: subject_line,
                    body: text.clone(),
                    interaction: None,
                    delivery_status: HashMap::new(),
                    response: None,
                    created_at: Utc::now(),
                    expires_at: None,
                    read: false,
                    thread_id: thread_id.clone().or_else(|| Some(task.id.to_string())),
                    reply_to_external_id: thread_id.clone(),
                };
                self.notification_router
                    .deliver_to_channel(msg, &target_id.to_string())
                    .await
            }
            _ => {
                let outbound = agentos_channels::types::OutboundMessage {
                    channel_instance_id: target_id.to_string(),
                    content: agentos_channels::types::MessageContent::Text(text.clone()),
                    thread_id: thread_id.clone(),
                };
                self.channel_manager
                    .send(&target_id.to_string(), outbound)
                    .await
                    .map(|_| ())
            }
        };

        match send_result {
            Ok(()) => {
                let preview: String = text.chars().take(120).collect();
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ChannelMessageSent,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "channel_id": target_id.to_string(),
                        "channel_name": target_name,
                        "kind": target_kind.to_string(),
                        "thread_id": thread_id,
                        "text_preview": preview,
                        "text_len": text.chars().count(),
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "status": "delivered",
                        "channel_id": target_id.to_string(),
                        "channel": target_name,
                        "kind": target_kind.to_string(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": e.to_string(),
                    "channel_id": target_id.to_string(),
                    "kind": target_kind.to_string(),
                }),
            },
        }
    }
}
