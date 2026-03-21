use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;
use std::collections::BTreeSet;
use std::time::Duration;

impl Kernel {
    pub(crate) async fn cmd_run_task(
        &self,
        agent_name: Option<String>,
        prompt: String,
        autonomous: bool,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent_id = match agent_name {
            Some(name) => match registry.get_by_name(&name) {
                Some(a) if a.status != AgentStatus::Offline => a.id,
                Some(_) => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' is offline", name),
                    }
                }
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' not found", name),
                    }
                }
            },
            None => {
                let agents: Vec<AgentProfile> =
                    registry.list_online().into_iter().cloned().collect();
                match self.router.route(&prompt, &agents).await {
                    Ok(id) => id,
                    Err(e) => {
                        return KernelResponse::Error {
                            message: format!("Failed to route task: {}", e),
                        }
                    }
                }
            }
        };

        let agent = match registry.get_by_id(&agent_id) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found after routing", agent_id),
                }
            }
        };
        let effective_permissions = registry.compute_effective_permissions(&agent_id);
        drop(registry);

        let task_id = TaskID::new();
        let task_timeout = if autonomous {
            Duration::from_secs(self.config.kernel.autonomous_mode.task_timeout_secs)
        } else {
            Duration::from_secs(self.config.kernel.default_task_timeout_secs)
        };
        let capability_token = match self.capability_engine.issue_token(
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
            effective_permissions,
            task_timeout,
        ) {
            Ok(token) => token,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to issue capability token: {}", e),
                };
            }
        };

        let reasoning_hints = Some(infer_reasoning_hints(&prompt));
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
            original_prompt: prompt,
            history: Vec::new(),
            parent_task: None,
            reasoning_hints,
            max_iterations: None,
            trigger_source: None,
            autonomous,
        };

        self.scheduler.register_external(task.clone()).await;
        self.scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Running)
            .await
            .ok();
        self.scheduler.mark_started(&task.id).await.ok();

        // Execute task synchronously so the CLI gets the result
        let trace_id = TraceID::new();
        let result = self.execute_task_sync(&task, &trace_id).await;
        match result {
            Ok(task_result) => {
                self.scheduler
                    .update_state_if_not_terminal(&task.id, TaskState::Complete)
                    .await
                    .ok();
                self.cleanup_task_subscriptions(&task.id).await;
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "task_id": task.id.to_string(),
                        "result": task_result.answer,
                    })),
                }
            }
            Err(e) => {
                let msg = e.to_string();
                let is_waiting = self
                    .scheduler
                    .get_task(&task.id)
                    .await
                    .map(|t| t.state == TaskState::Waiting)
                    .unwrap_or(false);
                let paused_by_message = msg.to_ascii_lowercase().starts_with("task paused:");
                if is_waiting || paused_by_message {
                    self.scheduler
                        .update_state_if_not_terminal(&task.id, TaskState::Waiting)
                        .await
                        .ok();
                    return KernelResponse::Success {
                        data: Some(serde_json::json!({
                            "task_id": task.id.to_string(),
                            "status": "paused",
                            "reason": msg,
                        })),
                    };
                }

                self.scheduler
                    .update_state_if_not_terminal(&task.id, TaskState::Failed)
                    .await
                    .ok();
                self.cleanup_task_subscriptions(&task.id).await;
                KernelResponse::Error { message: msg }
            }
        }
    }

    pub(crate) async fn cmd_list_tasks(&self) -> KernelResponse {
        let tasks = self.scheduler.list_tasks().await;
        KernelResponse::TaskList(tasks)
    }

    pub(crate) async fn cmd_get_task_logs(&self, task_id: TaskID) -> KernelResponse {
        match self.scheduler.get_task(&task_id).await {
            Some(task) => {
                let logs: Vec<String> = task
                    .history
                    .iter()
                    .map(|entry| {
                        format!(
                            "[{}] {:?} -> {:?}: {}",
                            entry.timestamp.format("%H:%M:%S"),
                            entry.intent_type,
                            entry.target,
                            entry.payload.schema
                        )
                    })
                    .collect();
                KernelResponse::TaskLogs(logs)
            }
            None => KernelResponse::Error {
                message: format!("Task '{}' not found", task_id),
            },
        }
    }

    pub(crate) async fn cmd_cancel_task(&self, task_id: TaskID) -> KernelResponse {
        match self
            .scheduler
            .update_state(&task_id, TaskState::Cancelled)
            .await
        {
            Ok(_) => {
                self.cleanup_task_subscriptions(&task_id).await;
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn handle_task_delegation(
        &self,
        parent_task: &AgentTask,
        target_agent_name: &str,
        prompt: &str,
        priority: u8,
        timeout_secs: u64,
    ) -> Result<serde_json::Value, AgentOSError> {
        let registry = self.agent_registry.read().await;
        let target = registry
            .get_by_name(target_agent_name)
            .ok_or_else(|| AgentOSError::AgentNotFound(target_agent_name.to_string()))?
            .clone();

        if target.status == AgentStatus::Offline {
            return Err(AgentOSError::AgentNotFound(format!(
                "Agent '{}' is offline",
                target_agent_name
            )));
        }

        let target_permissions = registry.compute_effective_permissions(&target.id);
        drop(registry);

        let child_permissions = parent_task.capability_token.permissions.clone();
        let effective_permissions = child_permissions.intersect(&target_permissions);

        let child_token = self.capability_engine.issue_token(
            TaskID::new(),
            target.id,
            parent_task.capability_token.allowed_tools.clone(),
            parent_task.capability_token.allowed_intents.clone(),
            effective_permissions,
            Duration::from_secs(timeout_secs),
        )?;

        let child_task = AgentTask {
            id: child_token.task_id,
            state: TaskState::Queued,
            agent_id: target.id,
            capability_token: child_token,
            assigned_llm: None,
            priority,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(timeout_secs),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(parent_task.id),
            reasoning_hints: Some(infer_reasoning_hints(prompt)),
            max_iterations: None,
            trigger_source: None,
            // Child tasks inherit the parent's autonomous mode so long-running
            // orchestrators don't have their sub-agents capped arbitrarily.
            autonomous: parent_task.autonomous,
        };

        // Check for circular dependencies before enqueuing
        if let Err(reason) = self
            .scheduler
            .check_delegation_safe(parent_task.id, child_task.id)
            .await
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "task_delegation".to_string(),
                operation: reason,
            });
        }

        let _ = self.scheduler.enqueue(child_task.clone()).await;

        // Register the dependency: parent waits on child
        self.scheduler
            .add_dependency(parent_task.id, child_task.id)
            .await;

        // Emit TaskDelegated from the parent's perspective
        self.emit_event(
            EventType::TaskDelegated,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "parent_task_id": parent_task.id.to_string(),
                "child_task_id": child_task.id.to_string(),
                "parent_agent_id": parent_task.agent_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent_name,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            0,
        )
        .await;

        // Emit DelegationReceived from the target agent's perspective
        self.emit_event(
            EventType::DelegationReceived,
            EventSource::TaskScheduler,
            EventSeverity::Info,
            serde_json::json!({
                "child_task_id": child_task.id.to_string(),
                "parent_task_id": parent_task.id.to_string(),
                "delegating_agent_id": parent_task.agent_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent_name,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            0,
        )
        .await;

        Ok(serde_json::json!({
            "delegated_to": target_agent_name,
            "child_task_id": child_task.id.to_string(),
            "status": "queued",
        }))
    }
}

/// Infer reasoning hints from a prompt's characteristics.
fn infer_reasoning_hints(prompt: &str) -> TaskReasoningHints {
    let word_count = prompt.split_whitespace().count();

    let complexity = if word_count > 200 {
        ComplexityLevel::High
    } else if word_count > 50 {
        ComplexityLevel::Medium
    } else {
        ComplexityLevel::Low
    };

    let preemption = match complexity {
        ComplexityLevel::High => PreemptionLevel::High,
        ComplexityLevel::Medium => PreemptionLevel::Normal,
        ComplexityLevel::Low => PreemptionLevel::Low,
    };

    let preferred_turns = match complexity {
        ComplexityLevel::High => Some(10),
        ComplexityLevel::Medium => Some(5),
        ComplexityLevel::Low => Some(3),
    };

    TaskReasoningHints {
        estimated_complexity: complexity,
        preferred_turns,
        preemption_sensitivity: preemption,
    }
}
