//! Capability dispatch bridge — connects tools to capability providers.
//!
//! Implements the `CapabilityDispatcher` trait from `agentos-types` so that
//! KMC bridge tools (env-install, proc-spawn, etc.) can execute provider
//! actions without depending on `agentos-kernel` directly.

use crate::capability_provider::CapabilityContext;
use crate::capability_registry::CapabilityRegistry;
use agentos_audit::AuditLog;
use agentos_types::*;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Kernel-side implementation of `CapabilityDispatcher`.
///
/// Looks up the provider in the registry, constructs a `CapabilityContext`,
/// and calls the provider's `execute` method. Logs audit events for every
/// capability execution.
pub struct KernelCapabilityDispatcher {
    registry: Arc<RwLock<CapabilityRegistry>>,
    audit: Arc<AuditLog>,
}

impl KernelCapabilityDispatcher {
    pub fn new(registry: Arc<RwLock<CapabilityRegistry>>, audit: Arc<AuditLog>) -> Self {
        Self { registry, audit }
    }
}

#[async_trait::async_trait]
impl CapabilityDispatcher for KernelCapabilityDispatcher {
    async fn dispatch(
        &self,
        request: agentos_types::CapabilityDispatchRequest,
    ) -> Result<serde_json::Value, AgentOSError> {
        let agentos_types::CapabilityDispatchRequest {
            domain,
            action,
            params,
            agent_id,
            task_id,
            trace_id,
            data_dir,
            permissions,
            workspace_paths,
        } = request;
        // Look up the provider.
        let registry = self.registry.read().await;
        let provider = registry
            .get(&domain)
            .cloned()
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("unknown capability domain '{domain}'"),
            })?;
        drop(registry); // Release the read lock before executing.

        // Verify this action is supported.
        let required_perms =
            provider
                .required_permissions(&action)
                .ok_or_else(|| AgentOSError::KernelError {
                    reason: format!("unknown action '{action}' for domain '{domain}'"),
                })?;

        // Check permissions.
        for (resource, op) in &required_perms {
            if !permissions.check(resource, *op) {
                return Err(AgentOSError::PermissionDenied {
                    resource: resource.clone(),
                    operation: format!("capability {domain}.{action} requires {resource}:{op:?}"),
                });
            }
        }

        // Audit + metrics: capability requested.
        crate::metrics::record_capability_request(&domain, &action);
        let _ = self.audit.append(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::CapabilityRequested,
            agent_id: Some(agent_id),
            task_id: Some(task_id),
            tool_id: None,
            details: serde_json::json!({
                "domain": domain,
                "action": action,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        // Build context and execute.
        let context = CapabilityContext {
            agent_id,
            task_id,
            trace_id,
            data_dir,
            permissions,
            workspace_paths,
        };

        let result = provider.execute(&action, params, &context).await;

        // Audit + metrics: capability executed or failed.
        match &result {
            Ok(cap_result) => {
                crate::metrics::record_capability_success(&domain, &action);
                let _ = self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::CapabilityExecuted,
                    agent_id: Some(agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: cap_result.audit_metadata.clone(),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                Ok(cap_result.output.clone())
            }
            Err(e) => {
                crate::metrics::record_capability_failure(&domain, &action);
                let _ = self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::CapabilityFailed,
                    agent_id: Some(agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "domain": domain,
                        "action": action,
                        "error": format!("{e}"),
                    }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
                Err(e.clone())
            }
        }
    }
}
