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
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Target agent '{}' not found", to)
                    }),
                };
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

        let msg = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Agent(task.agent_id),
            task_id: Some(task.id),
            trace_id,
            kind: UserMessageKind::Notification,
            priority: priority_parsed,
            subject: subject.chars().take(80).collect(),
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
            subject: question.chars().take(80).collect(),
            body: question.clone(),
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
