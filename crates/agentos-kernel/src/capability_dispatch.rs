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
    /// Dynamic capability policy engine (W2). Consulted after the static token
    /// permission check; a `Deny`/`Escalate` rule blocks the capability. With
    /// the default `off` profile it allows everything (no behavior change).
    policy_engine: Arc<RwLock<crate::policy_engine::PolicyEngine>>,
}

impl KernelCapabilityDispatcher {
    pub fn new(
        registry: Arc<RwLock<CapabilityRegistry>>,
        audit: Arc<AuditLog>,
        policy_engine: Arc<RwLock<crate::policy_engine::PolicyEngine>>,
    ) -> Self {
        Self {
            registry,
            audit,
            policy_engine,
        }
    }

    /// Derive a policy-evaluation resource string from request params. Policy
    /// rules match on resource patterns (paths, package names, URLs); this
    /// pulls the most relevant field, falling back to `"*"`.
    fn policy_resource(params: &serde_json::Value) -> String {
        for key in [
            "path", "package", "url", "command", "binary", "host", "name", "resource",
        ] {
            if let Some(s) = params.get(key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
        "*".to_string()
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

        // Check permissions. A denial here is a security-relevant event and
        // was previously unobservable (CapabilityDenied was never emitted).
        for (resource, op) in &required_perms {
            if !permissions.check(resource, *op) {
                let _ = self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::CapabilityDenied,
                    agent_id: Some(agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "domain": domain,
                        "action": action,
                        "resource": resource,
                        "operation": format!("{op:?}"),
                    }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
                return Err(AgentOSError::PermissionDenied {
                    resource: resource.clone(),
                    operation: format!("capability {domain}.{action} requires {resource}:{op:?}"),
                });
            }
        }

        // Dynamic policy enforcement (W2): the static token check above proves
        // the agent *holds* the permission; the policy engine decides whether
        // this specific (domain, action, resource) is allowed right now. With
        // the default `off` profile this always returns Allow.
        {
            let policy_resource = Self::policy_resource(&params);
            let effect =
                self.policy_engine
                    .read()
                    .await
                    .evaluate(&domain, &action, &policy_resource);
            use crate::policy_engine::PolicyEffect;
            if !matches!(effect, PolicyEffect::Allow) {
                let (severity, reason) = match effect {
                    PolicyEffect::Deny => (
                        agentos_audit::AuditSeverity::Warn,
                        "denied by security policy",
                    ),
                    PolicyEffect::Escalate => (
                        agentos_audit::AuditSeverity::Warn,
                        "requires operator approval (security policy)",
                    ),
                    PolicyEffect::Allow => unreachable!(),
                };
                let _ = self.audit.append(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::CapabilityDenied,
                    agent_id: Some(agent_id),
                    task_id: Some(task_id),
                    tool_id: None,
                    details: serde_json::json!({
                        "domain": domain,
                        "action": action,
                        "resource": policy_resource,
                        "policy_effect": format!("{effect:?}"),
                    }),
                    severity,
                    reversible: false,
                    rollback_ref: None,
                });
                return Err(AgentOSError::PermissionDenied {
                    resource: format!("{domain}.{action}"),
                    operation: format!(
                        "capability {domain}.{action} for '{policy_resource}' {reason}"
                    ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_provider::{CapabilityContext, CapabilityProvider, CapabilityResult};
    use crate::capability_registry::CapabilityRegistry;
    use crate::policy_engine::{PolicyEngine, PolicyRule};
    use agentos_types::{AgentID, PermissionOp, PermissionSet, TaskID, TraceID};

    /// Minimal provider in the `env` domain whose `install` action requires
    /// `env.install:x`.
    struct FakeEnvProvider;

    #[async_trait::async_trait]
    impl CapabilityProvider for FakeEnvProvider {
        fn domain(&self) -> &str {
            "env"
        }
        fn supported_actions(&self) -> &[&str] {
            &["install"]
        }
        fn required_permissions(&self, _action: &str) -> Option<Vec<(String, PermissionOp)>> {
            Some(vec![("env.install".to_string(), PermissionOp::Execute)])
        }
        async fn execute(
            &self,
            _action: &str,
            _params: serde_json::Value,
            _context: &CapabilityContext,
        ) -> Result<CapabilityResult, AgentOSError> {
            Ok(CapabilityResult {
                output: serde_json::json!({"installed": true}),
                audit_metadata: serde_json::json!({}),
            })
        }
        fn description(&self) -> &str {
            "fake env provider"
        }
    }

    fn dispatcher_with_policy(
        policy: PolicyEngine,
    ) -> (KernelCapabilityDispatcher, tempfile::TempDir) {
        let mut registry = CapabilityRegistry::new();
        registry.register(Arc::new(FakeEnvProvider)).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let audit = Arc::new(agentos_audit::AuditLog::open(&tmp.path().join("audit.db")).unwrap());
        let d = KernelCapabilityDispatcher::new(
            Arc::new(RwLock::new(registry)),
            audit,
            Arc::new(RwLock::new(policy)),
        );
        (d, tmp)
    }

    fn request() -> agentos_types::CapabilityDispatchRequest {
        // A token that DOES hold env.install:x — so the static check passes and
        // the policy engine is the only thing that can block.
        let mut perms = PermissionSet::new();
        perms.grant_op("env.install".to_string(), PermissionOp::Execute, None);
        agentos_types::CapabilityDispatchRequest {
            domain: "env".to_string(),
            action: "install".to_string(),
            params: serde_json::json!({"package": "flask"}),
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
            data_dir: std::env::temp_dir(),
            permissions: perms,
            workspace_paths: vec![],
        }
    }

    #[tokio::test]
    async fn off_profile_allows_capability() {
        // The default `off` profile is permissive: a held permission executes.
        let (d, _tmp) = dispatcher_with_policy(PolicyEngine::off_profile());
        let out = d.dispatch(request()).await.expect("off profile must allow");
        assert_eq!(out, serde_json::json!({"installed": true}));
    }

    #[tokio::test]
    async fn deny_policy_blocks_held_capability() {
        // Proves the policy engine is WIRED: even though the token holds
        // env.install:x, a Deny rule blocks the capability.
        let policy = PolicyEngine::new(
            vec![PolicyRule {
                id: "deny-env".into(),
                domains: vec!["env".into()],
                actions: vec!["*".into()],
                resource_pattern: "*".into(),
                effect: crate::policy_engine::PolicyEffect::Deny,
                priority: 100,
            }],
            crate::policy_engine::PolicyEffect::Allow,
        );
        let (d, _tmp) = dispatcher_with_policy(policy);
        let err = d
            .dispatch(request())
            .await
            .expect_err("deny policy must block the capability");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn escalate_default_blocks_unmatched_capability() {
        // A profile whose default is Escalate blocks anything not explicitly
        // allowed — proving unmatched requests don't silently pass.
        let policy = PolicyEngine::new(vec![], crate::policy_engine::PolicyEffect::Escalate);
        let (d, _tmp) = dispatcher_with_policy(policy);
        let err = d
            .dispatch(request())
            .await
            .expect_err("escalate default must block");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }
}
