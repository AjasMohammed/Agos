use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::schedule::TimerAction;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_create_timer(
        &self,
        name: String,
        delay_secs: u64,
        agent_name: String,
        action_json: String,
    ) -> KernelResponse {
        let action: TimerAction = match serde_json::from_str(&action_json) {
            Ok(a) => a,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Invalid timer action JSON: {}", e),
                }
            }
        };

        // Mirror the tool path: if agent_name looks like a UUID, resolve it to
        // the registered agent name so launch_timer_task can find it by name.
        let resolved_name = if let Ok(aid) = agent_name.parse::<AgentID>() {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&aid)
                .map(|a| a.name.clone())
                .unwrap_or(agent_name)
        } else {
            agent_name
        };

        let agent_name_for_audit = resolved_name.clone();
        match self
            .schedule_manager
            .create_timer(name.clone(), delay_secs, resolved_name, action, None)
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TimerCreated,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "timer_name": name,
                        "timer_id": id.to_string(),
                        "delay_secs": delay_secs,
                        "agent_name": agent_name_for_audit,
                        "source": "cli",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::TimerId(id)
            }
            Err(e) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TimerActionFailed,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "timer_name": name,
                        "action": "create_timer",
                        "source": "cli",
                        "error": e.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Error {
                    message: e.to_string(),
                }
            }
        }
    }

    pub(crate) async fn cmd_list_timers(&self) -> KernelResponse {
        KernelResponse::TimerList(self.schedule_manager.list_timers().await)
    }

    pub(crate) async fn cmd_cancel_timer(&self, name: String) -> KernelResponse {
        match self.schedule_manager.cancel_timer_by_name(&name).await {
            Ok(timer) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TimerCancelled,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "timer_name": timer.name,
                        "timer_id": timer.id.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success { data: None }
            }
            Err(e) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TimerActionFailed,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "timer_name": name,
                        "action": "cancel_timer",
                        "source": "cli",
                        "error": e.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Error {
                    message: e.to_string(),
                }
            }
        }
    }
}
