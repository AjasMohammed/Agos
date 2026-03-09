use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) fn parse_permission(perm: &str) -> Option<(String, bool, bool, bool)> {
        let parts: Vec<&str> = perm.splitn(2, ':').collect();
        if parts.len() != 2 {
            return None;
        }
        let resource = parts[0].to_string();
        let flags = parts[1];
        let read = flags.contains('r');
        let write = flags.contains('w');
        let execute = flags.contains('x');
        if !read && !write && !execute {
            return None;
        }
        Some((resource, read, write, execute))
    }

    pub(crate) async fn cmd_grant_permission(
        &self,
        agent_name: String,
        permission: String,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let (resource, read, write, execute) = match Self::parse_permission(&permission) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!(
                    "Invalid permission '{}'. Expected format: resource:rwx (e.g. fs.user_data:rw)",
                    permission
                ),
                }
            }
        };

        let mut perms = self
            .capability_engine
            .get_permissions(&agent.id)
            .unwrap_or_default();
        perms.grant(resource, read, write, execute, None);
        self.capability_engine
            .update_permissions(&agent.id, perms)
            .ok();

        self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::PermissionGranted,
                agent_id: Some(agent.id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "permission": permission, "agent_name": agent_name }),
                severity: agentos_audit::AuditSeverity::Info,
            });

        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_revoke_permission(
        &self,
        agent_name: String,
        permission: String,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let (resource, read, write, execute) = match Self::parse_permission(&permission) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!(
                    "Invalid permission '{}'. Expected format: resource:rwx (e.g. fs.user_data:rw)",
                    permission
                ),
                }
            }
        };

        let mut perms = self
            .capability_engine
            .get_permissions(&agent.id)
            .unwrap_or_default();
        perms.revoke(&resource, read, write, execute);
        self.capability_engine
            .update_permissions(&agent.id, perms)
            .ok();

        self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::PermissionRevoked,
                agent_id: Some(agent.id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "permission": permission, "agent_name": agent_name }),
                severity: agentos_audit::AuditSeverity::Info,
            });

        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_show_permissions(&self, agent_name: String) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let perms = self
            .capability_engine
            .get_permissions(&agent.id)
            .unwrap_or_default();
        KernelResponse::Permissions(perms)
    }

    pub(crate) async fn cmd_create_perm_profile(
        &self,
        name: String,
        description: String,
        permissions_strs: Vec<String>,
    ) -> KernelResponse {
        let mut perms = PermissionSet::new();
        for p in permissions_strs {
            if let Some((res, r, w, x)) = Self::parse_permission(&p) {
                perms.grant(res, r, w, x, None);
            } else {
                return KernelResponse::Error {
                    message: format!("Invalid permission '{}'", p),
                };
            }
        }
        match self.profile_manager.create(&name, &description, perms) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_delete_perm_profile(&self, name: String) -> KernelResponse {
        match self.profile_manager.delete(&name) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_list_perm_profiles(&self) -> KernelResponse {
        let profiles = self.profile_manager.list_all();
        KernelResponse::PermProfileList(profiles)
    }

    pub(crate) async fn cmd_assign_perm_profile(
        &self,
        agent_name: String,
        profile_name: String,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent_id = match registry.get_by_name(&agent_name) {
            Some(a) => a.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let profile = match self.profile_manager.get(&profile_name) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Profile '{}' not found", profile_name),
                }
            }
        };

        let mut current_perms = self
            .capability_engine
            .get_permissions(&agent_id)
            .unwrap_or_default();
        for entry in profile.permissions.entries() {
            current_perms.grant(
                entry.resource.clone(),
                entry.read,
                entry.write,
                entry.execute,
                entry.expires_at,
            );
        }

        self.capability_engine
            .update_permissions(&agent_id, current_perms)
            .ok();
        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_grant_permission_timed(
        &self,
        agent_name: String,
        permission: String,
        expires_secs: u64,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let (resource, read, write, execute) = match Self::parse_permission(&permission) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Invalid permission"),
                }
            }
        };

        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_secs as i64);

        let mut perms = self
            .capability_engine
            .get_permissions(&agent.id)
            .unwrap_or_default();
        perms.grant(resource.clone(), read, write, execute, Some(expires_at));
        self.capability_engine
            .update_permissions(&agent.id, perms)
            .ok();

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionGranted,
            agent_id: Some(agent.id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "permission": permission, "expires_at": expires_at.to_rfc3339() }),
            severity: agentos_audit::AuditSeverity::Info,
        });

        KernelResponse::Success { data: None }
    }
}
