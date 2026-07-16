//! Kernel handlers for the durable agent org chart (`agentos org` CLI).
//!
//! These are thin wrappers over [`crate::org_store::OrgStore`]; the security
//! invariant (a node's scope must be a subset of its manager's) is enforced
//! inside `upsert_node`, not here.

use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::{OrgID, OrgNode, OrgNodeID, PermissionOp, PermissionSet, TeamRole};

/// Parse a scope entry of the form `<resource>:<flags>` where `flags ⊆ rwxqo`.
/// Splits on the **last** colon so path-style resources keep their own colons
/// (e.g. `fs:/home/u/docs/:r` → resource `fs:/home/u/docs/`, flags `r`).
fn parse_scope_entry(s: &str) -> Option<(String, bool, bool, bool, bool, bool)> {
    let idx = s.rfind(':')?;
    let resource = &s[..idx];
    let flags = &s[idx + 1..];
    // The flags segment must be non-empty and contain ONLY rwxqo. This rejects a
    // path-style resource with no trailing flags (e.g. `fs:/home/u/`), whose last
    // colon-segment is part of the path — not a flags spec — and would otherwise
    // be misread (e.g. "home" contains 'o', the observe flag).
    if resource.is_empty()
        || flags.is_empty()
        || !flags
            .chars()
            .all(|c| matches!(c, 'r' | 'w' | 'x' | 'q' | 'o'))
    {
        return None;
    }
    // At least one flag is guaranteed present by the guard above (non-empty and
    // all chars ∈ rwxqo), so no further emptiness check is needed.
    let read = flags.contains('r');
    let write = flags.contains('w');
    let execute = flags.contains('x');
    let query = flags.contains('q');
    let observe = flags.contains('o');
    Some((resource.to_string(), read, write, execute, query, observe))
}

fn role_str(role: &TeamRole) -> &'static str {
    match role {
        TeamRole::Coordinator => "coordinator",
        TeamRole::Worker => "worker",
    }
}

impl Kernel {
    pub(crate) async fn cmd_org_add_node(
        &self,
        org_id: String,
        agent_name: String,
        manager_node_id: Option<String>,
        role: String,
        title: String,
        scope: Vec<String>,
    ) -> KernelResponse {
        let Some(org_store) = &self.org_store else {
            return KernelResponse::Error {
                message: "org registry unavailable (org.db failed to open at boot)".to_string(),
            };
        };

        let org_id = match org_id.parse::<OrgID>() {
            Ok(o) => o,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("invalid org_id '{org_id}' (expected a UUID)"),
                }
            }
        };
        let manager_id = match manager_node_id {
            Some(s) => match s.parse::<OrgNodeID>() {
                Ok(m) => Some(m),
                Err(_) => {
                    return KernelResponse::Error {
                        message: format!("invalid manager_node_id '{s}' (expected a UUID)"),
                    }
                }
            },
            None => None,
        };
        let role = match role.to_lowercase().as_str() {
            "coordinator" => TeamRole::Coordinator,
            "worker" | "" => TeamRole::Worker,
            other => {
                return KernelResponse::Error {
                    message: format!("invalid role '{other}' (expected 'coordinator' or 'worker')"),
                }
            }
        };

        let mut cap_scope = PermissionSet::new();
        for entry in &scope {
            match parse_scope_entry(entry) {
                Some((resource, read, write, execute, query, observe)) => {
                    cap_scope.grant(resource.clone(), read, write, execute, None);
                    if query {
                        cap_scope.grant_op(resource.clone(), PermissionOp::Query, None);
                    }
                    if observe {
                        cap_scope.grant_op(resource, PermissionOp::Observe, None);
                    }
                }
                None => {
                    return KernelResponse::Error {
                        message: format!(
                            "invalid scope entry '{entry}' (expected '<resource>:<rwxqo>', e.g. 'fs:/home/u/docs/:r')"
                        ),
                    }
                }
            }
        }

        // Capture audit fields before the values move into `node`.
        let audit_agent_name = agent_name.clone();
        let audit_role = role_str(&role).to_string();
        let audit_scope = scope.clone();

        let node = OrgNode {
            node_id: OrgNodeID::new(),
            org_id,
            agent_name,
            manager_id,
            role,
            title,
            cap_scope,
            budget: None,
        };
        let node_id = node.node_id;
        match org_store.upsert_node(node).await {
            Ok(()) => {
                // Creating an org node sets a capability ceiling for an agent —
                // a security-relevant grant that must be auditable.
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::PermissionGranted,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "kind": "org_node_created",
                        "org_id": org_id.to_string(),
                        "node_id": node_id.to_string(),
                        "agent_name": audit_agent_name,
                        "role": audit_role,
                        "cap_scope": audit_scope,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "node_id": node_id.to_string(),
                        "org_id": org_id.to_string(),
                    })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("failed to add org node: {e}"),
            },
        }
    }

    pub(crate) async fn cmd_org_show(&self, org_id: String) -> KernelResponse {
        let Some(org_store) = &self.org_store else {
            return KernelResponse::Error {
                message: "org registry unavailable (org.db failed to open at boot)".to_string(),
            };
        };
        let org_id = match org_id.parse::<OrgID>() {
            Ok(o) => o,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("invalid org_id '{org_id}' (expected a UUID)"),
                }
            }
        };
        match org_store.load_org(&org_id).await {
            Ok(nodes) => {
                let arr: Vec<serde_json::Value> = nodes
                    .iter()
                    .map(|n| {
                        serde_json::json!({
                            "node_id": n.node_id.to_string(),
                            "agent_name": n.agent_name,
                            "manager_id": n.manager_id.as_ref().map(|m| m.to_string()),
                            "role": role_str(&n.role),
                            "title": n.title,
                            "has_budget": n.budget.is_some(),
                        })
                    })
                    .collect();
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "org_id": org_id.to_string(),
                        "nodes": arr,
                    })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("failed to load org: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_scope_entry;

    #[test]
    fn parse_scope_entry_handles_path_resources() {
        let (res, r, w, x, q, o) = parse_scope_entry("fs:/home/u/docs/:rw").unwrap();
        assert_eq!(res, "fs:/home/u/docs/");
        assert!(r && w && !x && !q && !o);

        let (res, _, _, x, _, _) = parse_scope_entry("process.exec:x").unwrap();
        assert_eq!(res, "process.exec");
        assert!(x);

        // No flags → rejected.
        assert!(parse_scope_entry("fs:/home/u/").is_none());
        // No colon → rejected.
        assert!(parse_scope_entry("bogus").is_none());
    }
}
