use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_create_schedule(
        &self,
        name: String,
        cron: String,
        agent_name: String,
        task: String,
        permissions: Vec<String>,
    ) -> KernelResponse {
        match self
            .schedule_manager
            .create_job(name.clone(), cron, agent_name, task, permissions)
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ScheduledJobCreated,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "job_name": name, "schedule_id": id }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::ScheduleId(id)
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_list_schedules(&self) -> KernelResponse {
        KernelResponse::ScheduleList(self.schedule_manager.list_jobs().await)
    }

    /// Resolve a schedule by name or UUID string.
    async fn resolve_schedule(&self, name: &str) -> Option<ScheduledJob> {
        if let Some(job) = self.schedule_manager.get_by_name(name).await {
            return Some(job);
        }
        if let Ok(id) = name.parse::<ScheduleID>() {
            return self.schedule_manager.get_job(&id).await;
        }
        None
    }

    pub(crate) async fn cmd_pause_schedule(&self, name: String) -> KernelResponse {
        if let Some(job) = self.resolve_schedule(&name).await {
            match self.schedule_manager.pause(&job.id).await {
                Ok(_) => {
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::ScheduledJobPaused,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "job_name": job.name }),
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
                message: format!("Schedule '{}' not found", name),
            }
        }
    }

    pub(crate) async fn cmd_resume_schedule(&self, name: String) -> KernelResponse {
        if let Some(job) = self.resolve_schedule(&name).await {
            match self.schedule_manager.resume(&job.id).await {
                Ok(_) => {
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::ScheduledJobResumed,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "job_name": job.name }),
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
                message: format!("Schedule '{}' not found", name),
            }
        }
    }

    pub(crate) async fn cmd_delete_schedule(&self, name: String) -> KernelResponse {
        if let Some(job) = self.resolve_schedule(&name).await {
            match self.schedule_manager.delete(&job.id).await {
                Ok(_) => {
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::ScheduledJobDeleted,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "job_name": job.name }),
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
                message: format!("Schedule '{}' not found", name),
            }
        }
    }
}
