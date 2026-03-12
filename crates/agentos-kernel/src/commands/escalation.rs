use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_list_escalations(&self, pending_only: bool) -> KernelResponse {
        let escalations = if pending_only {
            self.escalation_manager.list_pending().await
        } else {
            self.escalation_manager.list_all().await
        };

        let entries: Vec<serde_json::Value> = escalations
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "task_id": e.task_id.to_string(),
                    "agent_id": e.agent_id.to_string(),
                    "reason": format!("{:?}", e.reason),
                    "context_summary": e.context_summary,
                    "decision_point": e.decision_point,
                    "options": e.options,
                    "urgency": e.urgency,
                    "blocking": e.blocking,
                    "resolved": e.resolved,
                    "resolution": e.resolution,
                    "created_at": e.created_at.to_rfc3339(),
                })
            })
            .collect();

        KernelResponse::EscalationList(entries)
    }

    pub(crate) async fn cmd_get_escalation(&self, id: u64) -> KernelResponse {
        match self.escalation_manager.get(id).await {
            Some(e) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "id": e.id,
                    "task_id": e.task_id.to_string(),
                    "agent_id": e.agent_id.to_string(),
                    "reason": format!("{:?}", e.reason),
                    "context_summary": e.context_summary,
                    "decision_point": e.decision_point,
                    "options": e.options,
                    "urgency": e.urgency,
                    "blocking": e.blocking,
                    "resolved": e.resolved,
                    "resolution": e.resolution,
                    "created_at": e.created_at.to_rfc3339(),
                })),
            },
            None => KernelResponse::Error {
                message: format!("Escalation {} not found", id),
            },
        }
    }

    pub(crate) async fn cmd_resolve_escalation(&self, id: u64, decision: String) -> KernelResponse {
        match self.escalation_manager.resolve(id, decision.clone()).await {
            Some((task_id, blocking)) => {
                // If the escalation was blocking, resume the waiting task
                if blocking {
                    if let Err(e) = self
                        .scheduler
                        .update_state(&task_id, TaskState::Running)
                        .await
                    {
                        tracing::warn!(
                            task_id = %task_id,
                            error = %e,
                            "Failed to resume task after escalation resolve"
                        );
                    }
                }

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
                    agent_id: None,
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "escalation_resolved": id,
                        "decision": decision,
                        "blocking_resumed": blocking,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });

                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "status": "resolved",
                        "escalation_id": id,
                        "task_id": task_id.to_string(),
                        "task_resumed": blocking,
                    })),
                }
            }
            None => KernelResponse::Error {
                message: format!("Escalation {} not found or already resolved", id),
            },
        }
    }
}
