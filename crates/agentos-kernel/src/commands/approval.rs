//! Kernel-side handlers for approval-mode + learned policy bus commands.
//!
//! Mode changes mutate the live [`crate::hooks::ApprovalModeResolver`]
//! (which the registered hook holds a reference to), so they take effect on
//! the next tool call without restart. Mutations are also audited.

use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::{ApprovalMode, TraceID};

use crate::Kernel;

impl Kernel {
    pub(crate) async fn cmd_get_approval_config(&self) -> KernelResponse {
        match &self.approval_mode_resolver {
            Some(resolver) => {
                let snap = resolver.snapshot();
                let overrides = snap
                    .agent_overrides
                    .into_iter()
                    .map(|(k, v)| (k, v.to_string()))
                    .collect();
                KernelResponse::ApprovalConfigSnapshot {
                    mode: snap.mode.to_string(),
                    agent_overrides: overrides,
                }
            }
            None => KernelResponse::Error {
                message: "Approval mode resolver not initialized".into(),
            },
        }
    }

    pub(crate) async fn cmd_set_approval_mode(&self, mode_str: String) -> KernelResponse {
        let mode = match ApprovalMode::parse(&mode_str) {
            Ok(m) => m,
            Err(e) => return KernelResponse::Error { message: e },
        };
        let resolver = match &self.approval_mode_resolver {
            Some(r) => r.clone(),
            None => {
                return KernelResponse::Error {
                    message: "Approval mode resolver not initialized".into(),
                };
            }
        };
        let mut snap = resolver.snapshot();
        snap.mode = mode;
        resolver.reload(snap);
        self.audit_log(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::KernelConfigChanged,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "setting": "approval.mode",
                "new_value": mode.to_string(),
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });
        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_set_approval_agent_override(
        &self,
        agent_name: String,
        mode_str: String,
    ) -> KernelResponse {
        let mode = match ApprovalMode::parse(&mode_str) {
            Ok(m) => m,
            Err(e) => return KernelResponse::Error { message: e },
        };
        let resolver = match &self.approval_mode_resolver {
            Some(r) => r.clone(),
            None => {
                return KernelResponse::Error {
                    message: "Approval mode resolver not initialized".into(),
                };
            }
        };
        let mut snap = resolver.snapshot();
        snap.agent_overrides.insert(agent_name.clone(), mode);
        resolver.reload(snap);
        self.audit_log(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::KernelConfigChanged,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "setting": "approval.agent_overrides",
                "agent_name": agent_name,
                "new_value": mode.to_string(),
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });
        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_clear_approval_agent_override(
        &self,
        agent_name: String,
    ) -> KernelResponse {
        let resolver = match &self.approval_mode_resolver {
            Some(r) => r.clone(),
            None => {
                return KernelResponse::Error {
                    message: "Approval mode resolver not initialized".into(),
                };
            }
        };
        let mut snap = resolver.snapshot();
        let removed = snap.agent_overrides.remove(&agent_name).is_some();
        if removed {
            resolver.reload(snap);
            self.audit_log(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::KernelConfigChanged,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "setting": "approval.agent_overrides",
                    "agent_name": agent_name,
                    "action": "cleared",
                }),
                severity: AuditSeverity::Info,
                reversible: true,
                rollback_ref: None,
            });
        }
        KernelResponse::Success {
            data: Some(serde_json::json!({ "removed": removed })),
        }
    }

    pub(crate) async fn cmd_add_approval_policy(
        &self,
        tool_name: String,
        path_glob: Option<String>,
        agent_name: Option<String>,
    ) -> KernelResponse {
        let matcher = match &self.approval_policy_matcher {
            Some(m) => m.clone(),
            None => {
                return KernelResponse::Error {
                    message: "Approval policy store not available".into(),
                };
            }
        };
        // Resolve agent name → id if specified
        let agent_id = if let Some(name) = &agent_name {
            match self.agent_registry.read().await.get_by_name(name) {
                Some(a) => Some(a.id),
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent not found: {name}"),
                    };
                }
            }
        } else {
            None
        };
        match matcher.add(
            &tool_name,
            path_glob.as_deref(),
            agent_id,
            "local-cli",
            "bus",
            None,
        ) {
            Ok(entry) => {
                // Persistent "allow always" entries are higher-impact than
                // mode mutations (they outlive the operator session). Audit
                // every add — reused KernelConfigChanged with a structured
                // `setting` discriminator pending dedicated audit variants.
                self.audit_log(AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: AuditEventType::KernelConfigChanged,
                    agent_id: entry.agent_id,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "setting": "approval.policy.added",
                        "id": entry.id,
                        "tool_name": entry.tool_name,
                        "path_glob": entry.path_glob,
                        "agent_id": entry.agent_id.map(|a| a.to_string()),
                        "source": entry.source,
                        "granted_by": entry.granted_by,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: None,
                });
                KernelResponse::ApprovalPolicyAdded {
                    id: entry.id,
                    tool_name: entry.tool_name,
                    path_glob: entry.path_glob,
                    agent_name,
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to add approval policy: {e}"),
            },
        }
    }

    pub(crate) async fn cmd_revoke_approval_policy(&self, id: i64) -> KernelResponse {
        let matcher = match &self.approval_policy_matcher {
            Some(m) => m.clone(),
            None => {
                return KernelResponse::Error {
                    message: "Approval policy store not available".into(),
                };
            }
        };
        match matcher.revoke(id) {
            Ok(ok) => {
                if ok {
                    self.audit_log(AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: AuditEventType::KernelConfigChanged,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({
                            "setting": "approval.policy.revoked",
                            "id": id,
                        }),
                        severity: AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    });
                }
                KernelResponse::ApprovalPolicyRevoked { ok }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to revoke approval policy: {e}"),
            },
        }
    }

    pub(crate) async fn cmd_list_approval_policies(&self) -> KernelResponse {
        let matcher = match &self.approval_policy_matcher {
            Some(m) => m.clone(),
            None => {
                return KernelResponse::Error {
                    message: "Approval policy store not available (DB open failed at boot)".into(),
                };
            }
        };
        let entries = match matcher.list_all() {
            Ok(e) => e,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to list approval policies: {e}"),
                };
            }
        };
        let json_entries: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "tool_name": e.tool_name,
                    "path_glob": e.path_glob,
                    "agent_id": e.agent_id.map(|a| a.to_string()),
                    "granted_at": e.granted_at.to_rfc3339(),
                    "granted_by": e.granted_by,
                    "source": e.source,
                    "expires_at": e.expires_at.map(|d| d.to_rfc3339()),
                })
            })
            .collect();
        KernelResponse::ApprovalPolicyList(json_entries)
    }
}
