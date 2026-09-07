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
        let registry = self.agent_registry.read().await;
        // A UUID still has to name a registered agent. Accepting it unchecked
        // wrote a grant that `list_for_agent` could never match — silent no-op
        // folder access that looks live in the list, and that permanently
        // occupies the (path, agent) slot in the unique index.
        if let Ok(parsed) = agent_name.parse::<AgentID>() {
            return registry
                .get_by_id(&parsed)
                .map(|a| a.id)
                .ok_or(KernelResponse::Error {
                    message: format!("Agent not found: {agent_name}"),
                });
        }
        registry
            .get_by_name(agent_name)
            .map(|a| a.id)
            .ok_or(KernelResponse::Error {
                message: format!("Agent not found: {agent_name}"),
            })
    }

    /// `source` / `granted_by` record WHO created the grant — `("bus",
    /// "local-cli")` for the CLI, `("api", "<key name>")` for the REST surface.
    /// Handing out host filesystem access is exactly the event where "a remote
    /// key did this" and "someone typed it at a terminal" must not look alike
    /// in the audit log.
    pub(crate) async fn cmd_grant_workspace(
        &self,
        path: PathBuf,
        agent_name: Option<String>,
        mode: String,
        source: &str,
        granted_by: &str,
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
            .grant(&path, agent_id, parsed_mode, source, granted_by)
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
        revoked_by: &str,
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
                // A revoke that matched nothing is not a revocation. Auditing it
                // anyway filled the log with `WorkspaceRevoked {count: 0}` for
                // paths that had no grant, which reads as access being removed.
                if count == 0 {
                    return KernelResponse::WorkspaceGrantRevoked { count };
                }
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
                        "revoked_by": revoked_by,
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
