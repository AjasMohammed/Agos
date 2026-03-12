use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;
use std::collections::BTreeSet;
use std::time::Duration;

impl Kernel {
    pub(crate) async fn create_background_task(
        &self,
        name: String,
        agent_name: String,
        prompt: String,
        detached: bool,
    ) -> Result<TaskID, AgentOSError> {
        let registry = self.agent_registry.read().await;
        let agent = registry
            .get_by_name(&agent_name)
            .ok_or_else(|| AgentOSError::AgentNotFound(agent_name.clone()))?
            .clone();

        let target_permissions = registry.compute_effective_permissions(&agent.id);
        drop(registry);

        let task_id = TaskID::new();
        let capability_token = self
            .capability_engine
            .issue_token(
                task_id,
                agent.id,
                BTreeSet::new(),
                BTreeSet::from([
                    IntentTypeFlag::Read,
                    IntentTypeFlag::Write,
                    IntentTypeFlag::Execute,
                    IntentTypeFlag::Query,
                ]),
                target_permissions,
                Duration::from_secs(self.config.kernel.default_task_timeout_secs),
            )
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        let task = AgentTask {
            id: task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token,
            assigned_llm: Some(agent.id),
            priority: 5,
            created_at: chrono::Utc::now(),
            timeout: Duration::from_secs(self.config.kernel.default_task_timeout_secs),
            original_prompt: prompt.clone(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            trigger_source: None,
        };

        self.background_pool
            .register(BackgroundTask {
                id: task_id,
                name,
                agent_name,
                task_prompt: prompt,
                state: TaskState::Queued,
                started_at: None,
                completed_at: None,
                result: None,
                detached,
            })
            .await;

        let _ = self.scheduler.enqueue(task).await;

        Ok(task_id)
    }

    pub(crate) async fn cmd_run_background(
        &self,
        name: String,
        agent_name: String,
        task: String,
        detach: bool,
    ) -> KernelResponse {
        match self
            .create_background_task(name.clone(), agent_name, task, detach)
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::BackgroundTaskStarted,
                    agent_id: None,
                    task_id: Some(id),
                    tool_id: None,
                    details: serde_json::json!({ "bg_name": name }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success {
                    data: Some(serde_json::json!({ "task_id": id.to_string() })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_list_background(&self) -> KernelResponse {
        KernelResponse::BackgroundPoolList(self.background_pool.list_all().await)
    }

    pub(crate) async fn cmd_get_background_logs(
        &self,
        name: String,
        _follow: bool,
    ) -> KernelResponse {
        if let Some(task) = self.background_pool.get_by_name(&name).await {
            self.cmd_get_task_logs(task.id).await
        } else {
            KernelResponse::Error {
                message: format!("Background task '{}' not found", name),
            }
        }
    }

    pub(crate) async fn cmd_kill_background(&self, name: String) -> KernelResponse {
        if let Some(task) = self.background_pool.get_by_name(&name).await {
            match self
                .scheduler
                .update_state(&task.id, TaskState::Cancelled)
                .await
            {
                Ok(_) => {
                    self.background_pool
                        .fail(&task.id, "Killed by user".to_string())
                        .await;
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::BackgroundTaskKilled,
                        agent_id: None,
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({ "bg_name": name }),
                        severity: agentos_audit::AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    });
                    KernelResponse::Success { data: None }
                }
                Err(e) => KernelResponse::Error {
                    message: e.to_string(),
                },
            }
        } else {
            KernelResponse::Error {
                message: format!("Background task '{}' not found", name),
            }
        }
    }
}
