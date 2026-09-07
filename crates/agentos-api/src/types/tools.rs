use agentos_types::ToolID;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiToolSummary {
    #[schema(value_type = String)]
    pub id: ToolID,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub trust_tier: String,
    pub status: String,
    /// Approval risk class (`readonly_scoped`, `readonly_external`,
    /// `write_agent_state`, `write_scoped`, `exec_capable`, `control_plane`,
    /// `interactive`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_class: Option<String>,
    /// Permissions the tool requires (from `[capabilities_required]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct InstallToolRequest {
    pub manifest_path: String,
}
