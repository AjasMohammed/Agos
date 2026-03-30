use crate::Kernel;
use agentos_bus::message::KernelResponse;
use serde_json::json;
use tracing::error;

impl Kernel {
    pub(crate) async fn cmd_context_memory_read(&self, agent_id: String) -> KernelResponse {
        match self.context_memory_store.read(&agent_id).await {
            Ok(Some(entry)) => KernelResponse::Success {
                data: Some(json!({
                    "agent_id": entry.agent_id,
                    "content": entry.content,
                    "token_count": entry.token_count,
                    "version": entry.version,
                    "created_at": entry.created_at.to_rfc3339(),
                    "updated_at": entry.updated_at.to_rfc3339(),
                })),
            },
            Ok(None) => KernelResponse::Success {
                data: Some(json!({
                    "agent_id": agent_id,
                    "content": "",
                    "token_count": 0,
                    "version": 0,
                    "message": "No context memory set for this agent.",
                })),
            },
            Err(e) => {
                error!("Failed to read context memory for {}: {}", agent_id, e);
                KernelResponse::Error {
                    message: format!("Failed to read context memory: {}", e),
                }
            }
        }
    }

    pub(crate) async fn cmd_context_memory_update(
        &self,
        agent_id: String,
        content: String,
        reason: Option<String>,
    ) -> KernelResponse {
        match self
            .context_memory_store
            .write(&agent_id, &content, reason.as_deref())
            .await
        {
            Ok(entry) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ContextMemoryUpdated,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: json!({
                        "agent_id": entry.agent_id,
                        "version": entry.version,
                        "token_count": entry.token_count,
                        "reason": reason,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: Some(format!(
                        "context_memory:{}:{}",
                        entry.agent_id,
                        entry.version - 1
                    )),
                });
                KernelResponse::Success {
                    data: Some(json!({
                        "agent_id": entry.agent_id,
                        "version": entry.version,
                        "token_count": entry.token_count,
                    })),
                }
            }
            Err(e) => {
                error!("Failed to update context memory for {}: {}", agent_id, e);
                KernelResponse::Error {
                    message: format!("Failed to update context memory: {}", e),
                }
            }
        }
    }

    pub(crate) async fn cmd_context_memory_history(
        &self,
        agent_id: String,
        limit: u32,
    ) -> KernelResponse {
        match self
            .context_memory_store
            .history(&agent_id, limit as usize)
            .await
        {
            Ok(versions) => {
                let entries: Vec<_> = versions
                    .iter()
                    .map(|v| {
                        json!({
                            "version": v.version,
                            "token_count": v.token_count,
                            "updated_at": v.updated_at.to_rfc3339(),
                            "reason": v.reason,
                        })
                    })
                    .collect();
                KernelResponse::Success {
                    data: Some(json!({
                        "agent_id": agent_id,
                        "count": entries.len(),
                        "versions": entries,
                    })),
                }
            }
            Err(e) => {
                error!(
                    "Failed to get context memory history for {}: {}",
                    agent_id, e
                );
                KernelResponse::Error {
                    message: format!("Failed to get context memory history: {}", e),
                }
            }
        }
    }

    pub(crate) async fn cmd_context_memory_rollback(
        &self,
        agent_id: String,
        version: u32,
    ) -> KernelResponse {
        match self.context_memory_store.rollback(&agent_id, version).await {
            Ok(entry) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ContextMemoryUpdated,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: json!({
                        "agent_id": entry.agent_id,
                        "rolled_back_to": version,
                        "new_version": entry.version,
                        "token_count": entry.token_count,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: None,
                });
                KernelResponse::Success {
                    data: Some(json!({
                        "agent_id": entry.agent_id,
                        "rolled_back_to": version,
                        "new_version": entry.version,
                        "token_count": entry.token_count,
                    })),
                }
            }
            Err(e) => {
                error!("Failed to rollback context memory for {}: {}", agent_id, e);
                KernelResponse::Error {
                    message: format!("Failed to rollback context memory: {}", e),
                }
            }
        }
    }

    pub(crate) async fn cmd_context_memory_clear(&self, agent_id: String) -> KernelResponse {
        match self.context_memory_store.clear(&agent_id).await {
            Ok(()) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ContextMemoryUpdated,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: json!({
                        "agent_id": agent_id,
                        "action": "cleared",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: None,
                });
                KernelResponse::Success {
                    data: Some(json!({
                        "agent_id": agent_id,
                        "cleared": true,
                    })),
                }
            }
            Err(e) => {
                error!("Failed to clear context memory for {}: {}", agent_id, e);
                KernelResponse::Error {
                    message: format!("Failed to clear context memory: {}", e),
                }
            }
        }
    }

    pub(crate) async fn cmd_context_memory_set(
        &self,
        agent_id: String,
        content: String,
    ) -> KernelResponse {
        self.cmd_context_memory_update(agent_id, content, Some("Set via CLI".to_string()))
            .await
    }
}
