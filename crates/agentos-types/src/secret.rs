use crate::ids::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    pub id: SecretID,
    pub name: String,
    pub owner: SecretOwner,
    pub scope: SecretScope,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Note: SecretEntry never contains the actual secret value.
/// The encrypted value lives in the vault DB only.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecretOwner {
    Kernel,
    Agent(AgentID),
    Tool(ToolID),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecretScope {
    /// Accessible only by the kernel itself — agents cannot obtain proxy tokens for this scope.
    Kernel,
    /// Accessible by any agent.
    Global,
    /// Accessible only by the named agent.
    Agent(AgentID),
    /// Accessible only by the owning agent (for tool-scoped secrets).
    Tool(ToolID),
}

/// Metadata returned to CLI — never includes the actual value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMetadata {
    pub name: String,
    pub scope: SecretScope,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}
