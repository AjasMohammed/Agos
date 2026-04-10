use serde::{Deserialize, Serialize};

/// Enterprise RBAC roles for AgentOS.
///
/// Each role carries a fixed set of default allowed tools and method-level
/// capability flags.  These can be further refined by `DynamicPermissionRule`
/// or explicit `PermissionSet` overrides at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnterpriseRole {
    /// Full system control — can mint tokens, manage vault, view audit, execute tasks.
    Admin,
    /// Operational control — can execute tasks, manage agents, view audit, but cannot
    /// mint tokens or directly access the vault.
    Operator,
    /// Read-only compliance reviewer — can only view audit logs.
    Auditor,
    /// Autonomous agent identity — can execute tasks and use assigned tools.
    Agent,
    /// Passive observer — dashboard and stats read-only, no execution.
    Viewer,
}

impl EnterpriseRole {
    /// Returns the list of built-in tool names this role is permitted to invoke.
    pub fn default_allowed_tools(&self) -> Vec<&'static str> {
        match self {
            EnterpriseRole::Admin => vec![
                "file.read",
                "file.write",
                "shell.exec",
                "memory.read",
                "memory.write",
                "vault.read",
                "vault.write",
                "agent.spawn",
                "agent.kill",
                "audit.read",
            ],
            EnterpriseRole::Operator => vec![
                "file.read",
                "file.write",
                "shell.exec",
                "memory.read",
                "memory.write",
                "agent.spawn",
                "audit.read",
            ],
            EnterpriseRole::Auditor => vec!["audit.read"],
            EnterpriseRole::Agent => vec!["file.read", "file.write", "memory.read", "memory.write"],
            EnterpriseRole::Viewer => vec!["audit.read"],
        }
    }

    /// Whether this role is allowed to mint capability tokens.
    pub fn can_mint_tokens(&self) -> bool {
        matches!(self, EnterpriseRole::Admin)
    }

    /// Whether this role is allowed to read/write vault secrets.
    pub fn can_access_vault(&self) -> bool {
        matches!(self, EnterpriseRole::Admin)
    }

    /// Whether this role is allowed to submit or execute tasks.
    pub fn can_execute_tasks(&self) -> bool {
        matches!(
            self,
            EnterpriseRole::Admin | EnterpriseRole::Operator | EnterpriseRole::Agent
        )
    }

    /// Whether this role is allowed to read audit log entries.
    pub fn can_view_audit(&self) -> bool {
        matches!(
            self,
            EnterpriseRole::Admin
                | EnterpriseRole::Operator
                | EnterpriseRole::Auditor
                | EnterpriseRole::Viewer
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_has_all_capabilities() {
        let r = EnterpriseRole::Admin;
        assert!(r.can_mint_tokens());
        assert!(r.can_access_vault());
        assert!(r.can_execute_tasks());
        assert!(r.can_view_audit());
    }

    #[test]
    fn auditor_can_only_view_audit() {
        let r = EnterpriseRole::Auditor;
        assert!(!r.can_mint_tokens());
        assert!(!r.can_access_vault());
        assert!(!r.can_execute_tasks());
        assert!(r.can_view_audit());
        assert_eq!(r.default_allowed_tools(), vec!["audit.read"]);
    }

    #[test]
    fn viewer_cannot_execute_but_can_view_audit() {
        let r = EnterpriseRole::Viewer;
        assert!(!r.can_execute_tasks());
        assert!(r.can_view_audit());
        assert!(!r.can_access_vault());
    }

    #[test]
    fn agent_can_execute_but_not_mint_or_vault() {
        let r = EnterpriseRole::Agent;
        assert!(r.can_execute_tasks());
        assert!(!r.can_mint_tokens());
        assert!(!r.can_access_vault());
    }

    #[test]
    fn operator_cannot_access_vault_or_mint() {
        let r = EnterpriseRole::Operator;
        assert!(!r.can_mint_tokens());
        assert!(!r.can_access_vault());
        assert!(r.can_execute_tasks());
        assert!(r.can_view_audit());
    }

    #[test]
    fn role_serialization_roundtrip() {
        let role = EnterpriseRole::Operator;
        let json = serde_json::to_string(&role).unwrap();
        let decoded: EnterpriseRole = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, role);
    }

    #[test]
    fn admin_allowed_tools_contains_vault_and_audit() {
        let tools = EnterpriseRole::Admin.default_allowed_tools();
        assert!(tools.contains(&"vault.read"));
        assert!(tools.contains(&"audit.read"));
        assert!(tools.contains(&"agent.spawn"));
    }
}
