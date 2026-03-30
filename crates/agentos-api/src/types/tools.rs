use agentos_types::ToolID;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiToolSummary {
    pub id: ToolID,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub trust_tier: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallToolRequest {
    pub manifest_path: String,
}
