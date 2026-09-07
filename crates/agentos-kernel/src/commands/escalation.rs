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

    /// `actor` names who resolved it (`"local-cli"`, `"api-key:<name>"`) and
    /// becomes `granted_by` on a remembered grant.
    pub async fn cmd_resolve_escalation(
        &self,
        id: u64,
        decision: String,
        remember: bool,
        actor: &str,
    ) -> KernelResponse {
        // Snapshot before resolving: `resolve` only hands back ids, and the
        // metadata is what "remember" is minted from.
        let snapshot = if remember {
            self.escalation_manager.get(id).await
        } else {
            None
        };
        match self.escalation_manager.resolve(id, decision.clone()).await {
            Some((task_id, agent_id, blocking)) => {
                let approved = crate::escalation::resolution_is_approval(&decision);
                let remembered = self.remember_grant(approved, snapshot.as_ref(), actor);
                let mut task_resumed = false;
                let mut infra_failure = false;
                // If the escalation was blocking, resume the waiting task.
                // Chat turns and MCP-gateway calls gate through
                // `enforce_tool_pre` with a synthetic task id that the
                // scheduler never sees; their waiter is woken by the
                // resolution channel above, so there is nothing to requeue.
                let in_process_waiter = self.scheduler.get_task(&task_id).await.is_none();
                if blocking && in_process_waiter {
                    // The gate's waiter is woken by the resolution channel; an
                    // approval really does let the call proceed, so report it
                    // (and audit it) as resumed rather than as a refusal.
                    task_resumed = approved;
                    tracing::info!(
                        escalation_id = id,
                        task_id = %task_id,
                        approved,
                        "Escalation resolved for an in-process tool gate (chat/gateway); waiter notified"
                    );
                } else if blocking {
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
                                        let reason = format!(
                                            "Escalation {} approved but requeue failed: {}",
                                            id, e
                                        );
                                        self.background_pool.fail(&task_id, reason.clone()).await;
                                        self.scheduler.set_failure_reason(&task_id, reason).await;
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
                                let reason =
                                    format!("Escalation {} denied with decision: {}", id, decision);
                                self.background_pool.fail(&task_id, reason.clone()).await;
                                self.scheduler.set_failure_reason(&task_id, reason).await;
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
                        "remembered": remembered.policy_id.is_some(),
                        "policy_id": remembered.policy_id,
                        "remember_note": remembered.note,
                    })),
                }
            }
            None => KernelResponse::Error {
                message: format!("Escalation {} not found or already resolved", id),
            },
        }
    }

    /// "Approve & remember": mint a standing grant from the escalation's
    /// `tool_approval` metadata. Never fails the resolve — the approval
    /// already went through; a missed grant just re-prompts.
    fn remember_grant(
        &self,
        approved: bool,
        esc: Option<&crate::escalation::PendingEscalation>,
        actor: &str,
    ) -> RememberSummary {
        use crate::approval_policy_store::RememberOutcome::*;
        let (true, Some(esc)) = (approved, esc) else {
            return RememberSummary::none(if esc.is_some() { "not an approval" } else { "" });
        };
        let Some(matcher) = self.approval_policy_matcher.as_ref() else {
            tracing::warn!(
                escalation_id = esc.id,
                "approve & remember requested but no approval policy store is configured"
            );
            return RememberSummary::none("no approval policy store configured");
        };
        match crate::approval_policy_store::grant_from_escalation(matcher, esc, actor, &self.audit)
        {
            Ok(Granted(entry)) => {
                tracing::info!(
                    escalation_id = esc.id,
                    policy_id = entry.id,
                    tool = %entry.tool_name,
                    path_glob = ?entry.path_glob,
                    "Standing grant minted from escalation"
                );
                RememberSummary {
                    policy_id: Some(entry.id),
                    note: format!(
                        "remembered for `{}`{}",
                        entry.tool_name,
                        entry
                            .path_glob
                            .as_deref()
                            .map(|g| format!(" under {g}"))
                            .unwrap_or_else(|| " (all paths)".into())
                    ),
                }
            }
            Ok(AlreadyRemembered) => {
                RememberSummary::none("already remembered by an earlier grant")
            }
            Ok(NotApplicable(reason)) => RememberSummary::none(reason),
            Err(e) => {
                tracing::warn!(escalation_id = esc.id, error = %e, "approve & remember: grant failed");
                RememberSummary::none("grant failed — see kernel log")
            }
        }
    }
}

/// What `cmd_resolve_escalation` reports back about the remember request.
struct RememberSummary {
    policy_id: Option<i64>,
    note: String,
}

impl RememberSummary {
    fn none(note: &str) -> Self {
        Self {
            policy_id: None,
            note: note.to_string(),
        }
    }
}
