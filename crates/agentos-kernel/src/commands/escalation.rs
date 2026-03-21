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
            Some((task_id, agent_id, blocking)) => {
                let decision_lower = decision.to_ascii_lowercase();
                let approved = matches!(
                    decision_lower.as_str(),
                    "approve" | "approved" | "allow" | "allowed"
                );
                let mut task_resumed = false;
                let mut infra_failure = false;
                // If the escalation was blocking, resume the waiting task
                if blocking {
                    if approved {
                        match self.scheduler.requeue(&task_id).await {
                            Ok(()) => {
                                task_resumed = true;
                            }
                            Err(e) => {
                                infra_failure = true;
                                tracing::warn!(
                                    task_id = %task_id,
                                    error = %e,
                                    "Failed to requeue task after escalation approve; failing task"
                                );
                                let task_snapshot = self.scheduler.get_task(&task_id).await;
                                let can_transition_failed = task_snapshot
                                    .as_ref()
                                    .map(|t| {
                                        !matches!(
                                            t.state,
                                            TaskState::Complete
                                                | TaskState::Failed
                                                | TaskState::Cancelled
                                        )
                                    })
                                    .unwrap_or(false);
                                if can_transition_failed {
                                    let transitioned = self
                                        .scheduler
                                        .update_state_if_not_terminal(&task_id, TaskState::Failed)
                                        .await
                                        .unwrap_or(false);
                                    if !transitioned {
                                        tracing::warn!(
                                            task_id = %task_id,
                                            "Skipped failing task after approve requeue failure due to terminal state"
                                        );
                                    } else {
                                        self.background_pool
                                            .fail(
                                                &task_id,
                                                format!(
                                                    "Escalation {} approved but requeue failed: {}",
                                                    id, e
                                                ),
                                            )
                                            .await;
                                        self.emit_event(
                                            EventType::TaskFailed,
                                            EventSource::TaskScheduler,
                                            EventSeverity::Warning,
                                            serde_json::json!({
                                                "task_id": task_id.to_string(),
                                                "agent_id": agent_id.to_string(),
                                                "reason": "escalation_approve_requeue_failed",
                                                "error": format!("Escalation {} approved but requeue failed: {}", id, e),
                                            }),
                                            0,
                                        )
                                        .await;
                                        let waiters =
                                            self.scheduler.complete_dependency(task_id).await;
                                        for waiter_id in waiters {
                                            if let Err(e) = self.scheduler.requeue(&waiter_id).await
                                            {
                                                tracing::warn!(error = %e, waiter_id = %waiter_id, "Requeue failed after escalation approval — waiter will timeout naturally");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        let task_snapshot = self.scheduler.get_task(&task_id).await;
                        let can_transition_failed = task_snapshot
                            .as_ref()
                            .map(|t| {
                                !matches!(
                                    t.state,
                                    TaskState::Complete | TaskState::Failed | TaskState::Cancelled
                                )
                            })
                            .unwrap_or(false);
                        if can_transition_failed {
                            let transitioned = self
                                .scheduler
                                .update_state_if_not_terminal(&task_id, TaskState::Failed)
                                .await
                                .unwrap_or(false);
                            if !transitioned {
                                tracing::warn!(
                                    task_id = %task_id,
                                    "Skipped failing denied escalation task due to terminal state"
                                );
                            } else {
                                self.background_pool
                                    .fail(
                                        &task_id,
                                        format!(
                                            "Escalation {} denied with decision: {}",
                                            id, decision
                                        ),
                                    )
                                    .await;
                                self.emit_event(
                                    EventType::TaskFailed,
                                    EventSource::TaskScheduler,
                                    EventSeverity::Warning,
                                    serde_json::json!({
                                        "task_id": task_id.to_string(),
                                        "agent_id": agent_id.to_string(),
                                        "reason": "escalation_denied",
                                        "error": format!("Escalation {} denied with decision: {}", id, decision),
                                    }),
                                    0,
                                )
                                .await;
                                let waiters = self.scheduler.complete_dependency(task_id).await;
                                for waiter_id in waiters {
                                    if let Err(e) = self.scheduler.requeue(&waiter_id).await {
                                        tracing::warn!(error = %e, waiter_id = %waiter_id, "Requeue failed after escalation denial — waiter will timeout naturally");
                                    }
                                }
                            }
                        }
                    }
                }

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: if task_resumed {
                        agentos_audit::AuditEventType::TaskStateChanged
                    } else if infra_failure {
                        agentos_audit::AuditEventType::TaskFailed
                    } else if approved && !blocking {
                        agentos_audit::AuditEventType::RiskEscalation
                    } else {
                        agentos_audit::AuditEventType::ActionForbidden
                    },
                    agent_id: Some(agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "escalation_id": id,
                        "decision": decision,
                        "task_resumed": task_resumed,
                        "blocking": blocking,
                    }),
                    severity: if task_resumed || (approved && !blocking) {
                        agentos_audit::AuditSeverity::Info
                    } else {
                        agentos_audit::AuditSeverity::Warn
                    },
                    reversible: false,
                    rollback_ref: None,
                });

                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "status": "resolved",
                        "escalation_id": id,
                        "task_id": task_id.to_string(),
                        "task_resumed": task_resumed,
                    })),
                }
            }
            None => KernelResponse::Error {
                message: format!("Escalation {} not found or already resolved", id),
            },
        }
    }
}
