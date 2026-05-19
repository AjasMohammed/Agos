//! Kernel-side handlers for user filesystem grant commands.
//!
//! Grants record which host directories an agent (or every agent) may
//! read/write/exec. The CLI/web call these through the bus; the kernel updates
//! the [`crate::workspace_grant_store::WorkspaceGrantRegistry`] and emits audit
//! events.

use std::path::PathBuf;

use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::{AgentID, TraceID, WorkspaceGrantMode};

use crate::Kernel;

impl Kernel {
    /// Accept either a registered agent display name or a raw `AgentID` UUID.
    async fn resolve_agent_for_workspace(
        &self,
        agent_name: &str,
    ) -> Result<AgentID, KernelResponse> {
        if let Ok(parsed) = agent_name.parse::<AgentID>() {
            return Ok(parsed);
        }
        self.agent_registry
            .read()
            .await
            .get_by_name(agent_name)
            .map(|a| a.id)
            .ok_or(KernelResponse::Error {
                message: format!("Agent not found: {agent_name}"),
            })
    }

    pub(crate) async fn cmd_grant_workspace(
        &self,
        path: PathBuf,
        agent_name: Option<String>,
        mode: String,
    ) -> KernelResponse {
        let parsed_mode = match WorkspaceGrantMode::parse(&mode) {
            Ok(m) => m,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("invalid mode '{mode}': {e}"),
                };
            }
        };
        let agent_id = match agent_name.as_deref() {
            Some(name) => match self.resolve_agent_for_workspace(name).await {
                Ok(id) => Some(id),
                Err(resp) => return resp,
            },
            None => None,
        };
        match self
            .workspace_grants
            .grant(&path, agent_id, parsed_mode, "bus", "local-cli")
        {
            Ok(grant) => {
                self.audit_log(AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: AuditEventType::WorkspaceGranted,
                    agent_id,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "id": grant.id,
                        "path": grant.path.to_string_lossy(),
                        "agent_id": grant.agent_id.as_ref().map(|a| a.to_string()),
                        "mode": grant.mode.to_string(),
                        "source": grant.source,
                        "granted_by": grant.granted_by,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: None,
                });
                KernelResponse::WorkspaceGrantCreated(grant)
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to grant workspace: {e}"),
            },
        }
    }

    pub(crate) async fn cmd_revoke_workspace(
        &self,
        path: PathBuf,
        agent_name: Option<String>,
    ) -> KernelResponse {
        let agent_id = match agent_name.as_deref() {
            Some(name) => match self.resolve_agent_for_workspace(name).await {
                Ok(id) => Some(id),
                Err(resp) => return resp,
            },
            None => None,
        };
        match self.workspace_grants.revoke(&path, agent_id.as_ref()) {
            Ok(count) => {
                self.audit_log(AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: AuditEventType::WorkspaceRevoked,
                    agent_id,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "path": path.to_string_lossy(),
                        "count": count,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::WorkspaceGrantRevoked { count }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to revoke workspace: {e}"),
            },
        }
    }

    pub(crate) async fn cmd_list_workspace_grants(
        &self,
        agent_name: Option<String>,
    ) -> KernelResponse {
        let grants = match agent_name {
            Some(name) => match self.resolve_agent_for_workspace(&name).await {
                Ok(id) => self.workspace_grants.list_for_agent(&id),
                Err(resp) => return resp,
            },
            None => self.workspace_grants.list_all_active(),
        };
        KernelResponse::WorkspaceGrantList(grants)
    }
}
