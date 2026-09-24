use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    /// Parse a permission string like `"fs.data:rwq"` into individual op flags.
    ///
    /// Supported flag characters: r=Read, w=Write, x=Execute, q=Query, o=Observe.
    /// Parse `resource:BITS`. Resources themselves contain colons
    /// (`fs:agents/<name>/`, `net:`), so the bits are whatever follows the
    /// *last* colon, and must be drawn from `r w x q o` only — otherwise
    /// `fs:/data/:rw` would parse as resource `fs` with bits `/data/:rw`.
    pub fn parse_permission(perm: &str) -> Option<(String, bool, bool, bool, bool, bool)> {
        let (resource, flags) = perm.rsplit_once(':')?;
        if resource.is_empty()
            || flags.is_empty()
            || !flags
                .chars()
                .all(|c| matches!(c, 'r' | 'w' | 'x' | 'q' | 'o'))
        {
            return None;
        }
        Some((
            resource.to_string(),
            flags.contains('r'),
            flags.contains('w'),
            flags.contains('x'),
            flags.contains('q'),
            flags.contains('o'),
        ))
    }

    /// Canonicalize a `skill:`-namespaced resource typed by an operator.
    ///
    /// `skill_permission_resource` lowercases the skill name and terminates it
    /// with `/`; an operator string does neither, so `skill:Researcher/` and
    /// `skill:researcher` (no terminator) would be stored as resources that
    /// `check()` never matches — a grant that grants nothing, or a deny that
    /// prefix-matches every sibling skill (`skill:researcher` also covers
    /// `researcher-pro`). This is the equivalent of MCP's `sanitize_tool_name`.
    ///
    /// The bare namespace `skill:` (the broad default grant) is left alone.
    /// Every other resource is returned unchanged.
    pub(crate) fn canonicalize_permission_resource(resource: &str) -> String {
        match resource.strip_prefix("skill:") {
            Some(name) if !name.is_empty() => skill_permission_resource(name.trim_end_matches('/')),
            _ => resource.to_string(),
        }
    }

    pub(crate) async fn cmd_grant_permission(
        &self,
        agent_name: String,
        permission: String,
    ) -> KernelResponse {
        let (resource, read, write, execute, query, observe) =
            match Self::parse_permission(&permission) {
                Some(p) => p,
                None => {
                    return KernelResponse::Error {
                        message: format!(
                    "Invalid permission '{}'. Expected format: resource:BITS (r,w,x,q,o e.g. fs.user_data:rw, memory.semantic:rq)",
                    permission
                ),
                    }
                }
            };

        let mut registry = self.agent_registry.write().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };

        let resource = Self::canonicalize_permission_resource(&resource);
        let mut perms = agent.permissions.clone();
        // An explicit deny outranks every grant, so granting a resource that
        // is currently denied would silently change nothing. A grant is the
        // operator reversing that decision — e.g. re-enabling a skill they
        // scoped out with `perm revoke <agent> skill:<name>/:x`.
        let cleared_deny = perms.clear_deny(&resource);
        perms.grant(resource.clone(), read, write, execute, None);
        if query {
            perms.grant_op(resource.clone(), PermissionOp::Query, None);
        }
        if observe {
            perms.grant_op(resource.clone(), PermissionOp::Observe, None);
        }
        if let Err(e) = registry.update_agent_permissions(&agent.id, perms) {
            return KernelResponse::Error {
                message: format!("Failed to update permissions: {e}"),
            };
        }
        drop(registry);

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionGranted,
            agent_id: Some(agent.id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "permission": permission,
                "agent_name": agent_name,
                "cleared_deny": cleared_deny,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        // Emit AgentPermissionGranted event
        self.emit_event(
            EventType::AgentPermissionGranted,
            EventSource::AgentLifecycle,
            EventSeverity::Info,
            serde_json::json!({
                "agent_id": agent.id.to_string(),
                "agent_name": agent_name,
                "permission": permission,
            }),
            0,
        )
        .await;

        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_revoke_permission(
        &self,
        agent_name: String,
        permission: String,
    ) -> KernelResponse {
        let (resource, read, write, execute, query, observe) =
            match Self::parse_permission(&permission) {
                Some(p) => p,
                None => {
                    return KernelResponse::Error {
                        message: format!(
                    "Invalid permission '{}'. Expected format: resource:BITS (r,w,x,q,o e.g. fs.user_data:rw, memory.semantic:rq)",
                    permission
                ),
                    }
                }
            };

        let mut registry = self.agent_registry.write().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };

        // Effective permissions also include role grants; those are not in
        // the agent's own set, so a revoke here would report success and
        // change nothing. Say so instead.
        let resource = Self::canonicalize_permission_resource(&resource);

        // A revoke can also narrow a broader direct grant: the default
        // `skill:` grant covers `skill:researcher/`, and scoping that one
        // skill out is a deny, not an entry deletion. Only a resource with
        // neither its own entry nor a covering direct grant is unrevokable
        // here (it comes from a role, or was never granted).
        let has_direct_entry = agent
            .permissions
            .entries()
            .iter()
            .any(|e| e.resource == resource);
        // Which ops the direct set currently confers on this resource, via
        // ANY grant. `check()` (not a raw prefix scan) so wildcards, the
        // path-boundary rule and expiry all behave as they do at call time.
        const ALL_OPS: [(PermissionOp, char); 5] = [
            (PermissionOp::Read, 'r'),
            (PermissionOp::Write, 'w'),
            (PermissionOp::Execute, 'x'),
            (PermissionOp::Query, 'q'),
            (PermissionOp::Observe, 'o'),
        ];
        let requested = [
            (read, PermissionOp::Read),
            (write, PermissionOp::Write),
            (execute, PermissionOp::Execute),
            (query, PermissionOp::Query),
            (observe, PermissionOp::Observe),
        ];
        let effective: Vec<(PermissionOp, char)> = ALL_OPS
            .into_iter()
            .filter(|(op, _)| agent.permissions.check(&resource, *op))
            .collect();
        let covered_by_broader_grant = !has_direct_entry && !effective.is_empty();
        if !has_direct_entry && !covered_by_broader_grant {
            return KernelResponse::Error {
                message: format!(
                    "Permission '{}' is not granted directly to '{}' (it comes from a role or is absent); edit the role instead",
                    permission, agent_name
                ),
            };
        }
        // Narrowing a broader grant is recorded as a deny, and a deny has no
        // notion of ops — it kills every op on the resource. Refuse a partial
        // revoke rather than silently taking away bits the operator kept.
        if covered_by_broader_grant {
            let kept: Vec<char> = effective
                .iter()
                .filter(|(op, _)| !requested.iter().any(|(on, r)| *on && r == op))
                .map(|(_, c)| *c)
                .collect();
            if !kept.is_empty() {
                let all: String = effective.iter().map(|(_, c)| *c).collect();
                return KernelResponse::Error {
                    message: format!(
                        "'{agent_name}' holds '{resource}' through a broader grant, which can only be narrowed by denying the whole resource — that would also drop '{}'. Re-run with every op it confers: '{resource}:{all}'",
                        kept.iter().collect::<String>(),
                    ),
                };
            }
        }

        let mut perms = agent.permissions.clone();
        perms.revoke(&resource, read, write, execute);
        if query {
            perms.revoke_op(&resource, PermissionOp::Query);
        }
        if observe {
            perms.revoke_op(&resource, PermissionOp::Observe);
        }
        // A fully-revoked default would otherwise be handed back on the next
        // connect: `revoke` deletes the entry once every bit clears, and
        // `backfill_late_default_grants` reads an absent entry as "never
        // granted". Record the operator's decision as a deny, which `check()`
        // honours ahead of any grant and the backfill skips.
        if !perms.entries().iter().any(|e| e.resource == resource)
            && (covered_by_broader_grant
                || crate::commands::agent::is_late_default_grant(&resource))
        {
            perms.deny(resource.clone());
        }
        if let Err(e) = registry.update_agent_permissions(&agent.id, perms) {
            return KernelResponse::Error {
                message: format!("Failed to update permissions: {e}"),
            };
        }
        drop(registry);

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionRevoked,
            agent_id: Some(agent.id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "permission": permission, "agent_name": agent_name }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        // Emit AgentPermissionRevoked event
        self.emit_event(
            EventType::AgentPermissionRevoked,
            EventSource::AgentLifecycle,
            EventSeverity::Warning,
            serde_json::json!({
                "agent_id": agent.id.to_string(),
                "agent_name": agent_name,
                "permission": permission,
            }),
            0,
        )
        .await;

        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_show_permissions(&self, agent_name: String) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.id,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        let perms = registry.compute_effective_permissions(&agent);
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
            if let Some((res, r, w, x, q, o)) = Self::parse_permission(&p) {
                perms.grant(res.clone(), r, w, x, None);
                if q {
                    perms.grant_op(res.clone(), PermissionOp::Query, None);
                }
                if o {
                    perms.grant_op(res, PermissionOp::Observe, None);
                }
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
        let profile = match self.profile_manager.get(&profile_name) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Profile '{}' not found", profile_name),
                }
            }
        };

        let mut registry = self.agent_registry.write().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };

        let mut current_perms = agent.permissions.clone();
        for entry in profile.permissions.entries() {
            current_perms.grant_entry(entry);
        }
        if let Err(e) = registry.update_agent_permissions(&agent.id, current_perms) {
            return KernelResponse::Error {
                message: format!("Failed to update permissions: {e}"),
            };
        }
        drop(registry);

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionGranted,
            agent_id: Some(agent.id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "profile_name": profile_name, "agent_name": agent_name }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_grant_permission_timed(
        &self,
        agent_name: String,
        permission: String,
        expires_secs: u64,
    ) -> KernelResponse {
        let (resource, read, write, execute, query, observe) =
            match Self::parse_permission(&permission) {
                Some(p) => p,
                None => {
                    return KernelResponse::Error {
                        message: "Invalid permission".to_string(),
                    }
                }
            };

        // Use saturating conversion: values > i64::MAX are clamped to a very large timeout
        // rather than wrapping to a negative number (which would create a past expiry).
        let expires_secs_i64 = i64::try_from(expires_secs).unwrap_or(i64::MAX / 2);
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_secs_i64);

        let mut registry = self.agent_registry.write().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };

        let resource = Self::canonicalize_permission_resource(&resource);
        let mut perms = agent.permissions.clone();
        // Same reason as the untimed grant: a deny outranks every grant, so a
        // timed re-grant of a denied resource would confer nothing.
        perms.clear_deny(&resource);
        perms.grant(resource.clone(), read, write, execute, Some(expires_at));
        if query {
            perms.grant_op(resource.clone(), PermissionOp::Query, Some(expires_at));
        }
        if observe {
            perms.grant_op(resource.clone(), PermissionOp::Observe, Some(expires_at));
        }
        if let Err(e) = registry.update_agent_permissions(&agent.id, perms) {
            return KernelResponse::Error {
                message: format!("Failed to update permissions: {e}"),
            };
        }
        drop(registry);

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionGranted,
            agent_id: Some(agent.id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "permission": permission, "expires_at": expires_at.to_rfc3339() }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::Success { data: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(r: bool, w: bool, x: bool, q: bool, o: bool) -> String {
        [(r, 'r'), (w, 'w'), (x, 'x'), (q, 'q'), (o, 'o')]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, c)| *c)
            .collect()
    }

    /// The API lists permissions as `resource:BITS` and hands them straight
    /// back to revoke; resources with colons must survive the round trip.
    #[test]
    fn permission_string_round_trips_colon_resources() {
        for s in [
            "fs:agents/nimo/:rwx",
            "net::rx",
            "fs.user_data:rw",
            "*:rwxqo",
        ] {
            let (res, r, w, x, q, o) = Kernel::parse_permission(s).unwrap();
            assert_eq!(format!("{res}:{}", bits(r, w, x, q, o)), s);
        }
        assert!(Kernel::parse_permission("fs:/home/user").is_none());
        assert!(Kernel::parse_permission("fs:/data/").is_none());
        assert!(Kernel::parse_permission(":rw").is_none());
        assert!(Kernel::parse_permission("fs").is_none());
    }

    /// An operator types the resource by hand; `skill_permission_resource`
    /// does not. Both must land on the same string or a grant grants nothing
    /// and a deny covers every sibling skill.
    #[test]
    fn skill_resources_are_canonicalized_to_the_enforced_form() {
        for typed in [
            "skill:researcher",
            "skill:researcher/",
            "skill:Researcher",
            "skill:RESEARCHER//",
        ] {
            assert_eq!(
                Kernel::canonicalize_permission_resource(typed),
                skill_permission_resource("researcher"),
                "{typed}"
            );
        }
        // The broad default grant and every non-skill resource pass through.
        assert_eq!(Kernel::canonicalize_permission_resource("skill:"), "skill:");
        assert_eq!(
            Kernel::canonicalize_permission_resource("fs:agents/Nimo/"),
            "fs:agents/Nimo/"
        );
    }
}
