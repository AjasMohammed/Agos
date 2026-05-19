use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::TraceID;

impl Kernel {
    pub(crate) async fn cmd_user_prefs_list_pending(&self, limit: u32) -> KernelResponse {
        match self.user_pref_proposal_store.list_pending(limit).await {
            Ok(rows) => KernelResponse::Success {
                data: Some(serde_json::json!({"proposals": rows})),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_user_prefs_accept(&self, proposal_id: String) -> KernelResponse {
        let Some(p) = (match self.user_pref_proposal_store.get(&proposal_id).await {
            Ok(v) => v,
            Err(e) => {
                return KernelResponse::Error {
                    message: e.to_string(),
                };
            }
        }) else {
            return KernelResponse::Error {
                message: format!("proposal not found: {proposal_id}"),
            };
        };

        // Order matters: claim the proposal *first*. Only on a successful
        // pending → accepted transition do we apply the side effect (memory
        // write). This prevents double-writes on retry after a partial failure.
        match self.user_pref_proposal_store.accept(&proposal_id).await {
            Ok(true) => {
                if let Err(e) = self
                    .context_memory_store
                    .write(
                        &p.agent_id.to_string(),
                        &format!("- {}", p.content),
                        Some("user_pref_proposal_accept"),
                    )
                    .await
                {
                    // Memory write failed after the proposal was claimed.
                    // Surface the error to the operator; the proposal stays
                    // accepted (operator can manually retry the memory write
                    // or just paste the content).
                    return KernelResponse::Error {
                        message: format!("proposal accepted but context-memory write failed: {e}"),
                    };
                }

                self.audit
                    .append(AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: AuditEventType::ProposalAccepted,
                        agent_id: Some(p.agent_id),
                        task_id: Some(p.task_id),
                        tool_id: None,
                        details: serde_json::json!({
                            "proposal_id": proposal_id,
                            "confidence": p.confidence,
                            "kind": p.kind,
                        }),
                        severity: AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    })
                    .ok();
                KernelResponse::Success {
                    data: Some(serde_json::json!({"accepted": true})),
                }
            }
            Ok(false) => KernelResponse::Error {
                message: "proposal already reviewed".to_string(),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_user_prefs_reject(&self, proposal_id: String) -> KernelResponse {
        // Snapshot the proposal up-front so we can record agent/task in the
        // audit entry — `reject()` only returns a bool.
        let proposal = match self.user_pref_proposal_store.get(&proposal_id).await {
            Ok(v) => v,
            Err(e) => {
                return KernelResponse::Error {
                    message: e.to_string(),
                };
            }
        };
        match self.user_pref_proposal_store.reject(&proposal_id).await {
            Ok(true) => {
                if let Some(p) = proposal {
                    self.audit
                        .append(AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: TraceID::new(),
                            event_type: AuditEventType::ProposalRejected,
                            agent_id: Some(p.agent_id),
                            task_id: Some(p.task_id),
                            tool_id: None,
                            details: serde_json::json!({
                                "proposal_id": proposal_id,
                                "confidence": p.confidence,
                            }),
                            severity: AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        })
                        .ok();
                }
                KernelResponse::Success {
                    data: Some(serde_json::json!({"rejected": true})),
                }
            }
            Ok(false) => KernelResponse::Error {
                message: "proposal not found or already reviewed".to_string(),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_user_prefs_stats(&self) -> KernelResponse {
        match self.user_pref_proposal_store.stats().await {
            Ok(stats) => KernelResponse::Success {
                data: Some(serde_json::json!({"stats": stats})),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }
}
