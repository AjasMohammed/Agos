//! Kernel command handlers for the structured user-profile store
//! (`agentos profile list/show/edit/forget`).
//!
//! Mirrors [`super::user_prefs`]: each handler returns a [`KernelResponse`] and
//! emits an audit entry on mutation. The store enforces caps/floors; these
//! handlers only translate bus commands and shape the response.

use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::{ProfileCategory, ProfilePatch, TraceID};

impl Kernel {
    pub(crate) async fn cmd_profile_list(&self, limit: u32) -> KernelResponse {
        match self.user_profile_store.list(limit).await {
            Ok(rows) => KernelResponse::Success {
                data: Some(serde_json::json!({ "entries": rows })),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_profile_show(&self, id: String) -> KernelResponse {
        match self.user_profile_store.get(&id).await {
            Ok(Some(entry)) => KernelResponse::Success {
                data: Some(serde_json::json!({ "entry": entry })),
            },
            Ok(None) => KernelResponse::Error {
                message: format!("profile entry not found: {id}"),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_profile_edit(
        &self,
        id: String,
        value: Option<String>,
        confidence: Option<f32>,
        category: Option<String>,
    ) -> KernelResponse {
        let patch = ProfilePatch {
            category: category.as_deref().map(ProfileCategory::from_str_lossy),
            value,
            confidence,
            pin_rank: None,
            status: None,
        };
        match self.user_profile_store.edit(&id, patch).await {
            Ok(true) => {
                self.audit
                    .append(AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: AuditEventType::ProfileEntryUpdated,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "id": id, "source": "command" }),
                        severity: AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    })
                    .ok();
                KernelResponse::Success {
                    data: Some(serde_json::json!({ "updated": true })),
                }
            }
            Ok(false) => KernelResponse::Error {
                message: format!("profile entry not found: {id}"),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_profile_forget(&self, id: String) -> KernelResponse {
        match self.user_profile_store.forget(&id).await {
            Ok(true) => {
                self.audit
                    .append(AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: AuditEventType::ProfileEntryRemoved,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({ "id": id, "source": "command" }),
                        severity: AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    })
                    .ok();
                KernelResponse::Success {
                    data: Some(serde_json::json!({ "forgotten": true })),
                }
            }
            Ok(false) => KernelResponse::Error {
                message: format!("profile entry not found: {id}"),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }
}
