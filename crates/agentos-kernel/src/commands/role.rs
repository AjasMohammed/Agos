use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_create_role(
        &self,
        role_name: String,
        description: String,
    ) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        if registry.get_role_by_name(&role_name).is_some() {
            return KernelResponse::Error {
                message: format!("Role '{}' already exists", role_name),
            };
        }
        let role = Role::new(role_name.clone(), description);
        registry.register_role(role);

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionGranted,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "action": "create_role", "role_name": role_name }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_delete_role(&self, role_name: String) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let role_id = match registry.get_role_by_name(&role_name) {
            Some(r) => r.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Role '{}' not found", role_name),
                }
            }
        };

        match registry.unregister_role(&role_id) {
            Ok(_) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::PermissionRevoked,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "action": "delete_role", "role_name": role_name }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error { message: e },
        }
    }

    pub(crate) async fn cmd_list_roles(&self) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let roles: Vec<Role> = registry.list_roles().into_iter().cloned().collect();
        KernelResponse::RoleList(roles)
    }

    pub(crate) async fn cmd_role_grant(
        &self,
        role_name: String,
        permission: String,
    ) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let mut perms = match registry.get_role_by_name(&role_name) {
            Some(r) => r.permissions.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Role '{}' not found", role_name),
                }
            }
        };

        let (resource, read, write, execute, query, observe) =
            match Self::parse_permission(&permission) {
                Some(p) => p,
                None => {
                    return KernelResponse::Error {
                        message: format!("Invalid permission '{}'", permission),
                    }
                }
            };

        perms.grant(resource.clone(), read, write, execute, None);
        if query {
            perms.grant_op(resource.clone(), PermissionOp::Query, None);
        }
        if observe {
            perms.grant_op(resource, PermissionOp::Observe, None);
        }
        if let Err(e) = registry.update_role_permissions(&role_name, perms) {
            return KernelResponse::Error { message: e };
        }

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionGranted,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "action": "role_grant", "role_name": role_name, "permission": permission }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_role_revoke(
        &self,
        role_name: String,
        permission: String,
    ) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let mut perms = match registry.get_role_by_name(&role_name) {
            Some(r) => r.permissions.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Role '{}' not found", role_name),
                }
            }
        };

        let (resource, read, write, execute, query, observe) =
            match Self::parse_permission(&permission) {
                Some(p) => p,
                None => {
                    return KernelResponse::Error {
                        message: format!("Invalid permission '{}'", permission),
                    }
                }
            };

        perms.revoke(&resource, read, write, execute);
        if query {
            perms.revoke_op(&resource, PermissionOp::Query);
        }
        if observe {
            perms.revoke_op(&resource, PermissionOp::Observe);
        }
        if let Err(e) = registry.update_role_permissions(&role_name, perms) {
            return KernelResponse::Error { message: e };
        }

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionRevoked,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "role_name": role_name, "permission": permission }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_assign_role(
        &self,
        agent_name: String,
        role_name: String,
    ) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let agent_id = match registry.get_by_name(&agent_name) {
            Some(a) => a.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };

        match registry.assign_role(&agent_id, role_name.clone()) {
            Ok(_) => {
                let (roles, perms) = match registry.get_by_id(&agent_id) {
                    Some(a) => (a.roles.clone(), Some(a.permissions.clone())),
                    None => (Vec::new(), None),
                };

                // Mirror the connect path's per-role observe grants, so a role
                // assigned to a running agent is not half-applied (subscribed by
                // the kernel, but unable to refine its own subscriptions) until
                // the agent happens to reconnect.
                if let Some(mut perms) = perms {
                    for resource in crate::event_bus::event_observe_permissions_for_role(&role_name)
                    {
                        perms.grant_op(resource.to_string(), PermissionOp::Observe, None);
                    }
                    if let Err(e) = registry.update_agent_permissions(&agent_id, perms) {
                        tracing::warn!(
                            agent_id = %agent_id,
                            role = %role_name,
                            error = %e,
                            "Failed to apply role event-observe permissions"
                        );
                    }
                }
                drop(registry);

                // Role-default event subscriptions are otherwise only seeded on
                // connect, so a role assigned to an already-running agent would
                // do nothing until it reconnected. Idempotent, so the reseed at
                // the next connect is a no-op.
                self.seed_role_subscriptions(agent_id, &roles).await;

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::PermissionGranted,
                    agent_id: Some(agent_id),
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "action": "assign_role", "role_name": role_name, "agent_name": agent_name }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error { message: e },
        }
    }

    pub(crate) async fn cmd_remove_role(
        &self,
        agent_name: String,
        role_name: String,
    ) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let agent_id = match registry.get_by_name(&agent_name) {
            Some(a) => a.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };

        match registry.remove_role(&agent_id, &role_name) {
            Ok(_) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::PermissionRevoked,
                    agent_id: Some(agent_id),
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "action": "remove_role", "role_name": role_name, "agent_name": agent_name }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error { message: e },
        }
    }
}
