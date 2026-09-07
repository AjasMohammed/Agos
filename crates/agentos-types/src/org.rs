//! Durable agent-organization types.
//!
//! An [`OrgNode`] makes the previously-ephemeral coordinator/worker relationship
//! (see [`crate::team`]) a persistent, queryable entity: each node binds an agent
//! to a manager (reporting line), a role, a capability scope, and an optional
//! budget. The capability scope of a node must always be a *subset* of its
//! manager's scope — enforced at write time via
//! [`crate::capability::PermissionSet::is_subset_of`] — so an org can never be
//! used to escalate a worker's privileges above the chain of command.

use crate::capability::PermissionSet;
use crate::ids::{OrgID, OrgNodeID};
use crate::task::AgentBudget;
use crate::team::TeamRole;
use serde::{Deserialize, Serialize};

/// A single node in an agent org chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgNode {
    /// Stable identifier for this node.
    pub node_id: OrgNodeID,
    /// Which org/company this node belongs to.
    pub org_id: OrgID,
    /// The registered agent name this node represents.
    pub agent_name: String,
    /// The node this one reports to. `None` marks the top of the org (the CEO).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager_id: Option<OrgNodeID>,
    /// Coordinator (delegates work) or Worker (executes it).
    pub role: TeamRole,
    /// Human-readable title, e.g. "Researcher". Empty when unset.
    #[serde(default)]
    pub title: String,
    /// Capability scope for this node. Must be a subset of the manager's scope.
    pub cap_scope: PermissionSet,
    /// Optional per-node budget. `None` inherits the global `[agent_budget]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<AgentBudget>,
}

impl OrgNode {
    /// Create a top-level (CEO) node with no manager.
    pub fn root(org_id: OrgID, agent_name: impl Into<String>, cap_scope: PermissionSet) -> Self {
        Self {
            node_id: OrgNodeID::new(),
            org_id,
            agent_name: agent_name.into(),
            manager_id: None,
            role: TeamRole::Coordinator,
            title: String::new(),
            cap_scope,
            budget: None,
        }
    }
}
