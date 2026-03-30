use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiPipelineSummary {
    pub name: String,
    pub description: Option<String>,
    pub step_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavePipelineRequest {
    pub name: String,
    pub definition: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPipelineRequest {
    pub name: String,
    pub input: String,
    #[serde(default)]
    pub detach: bool,
    #[serde(default)]
    pub agent_name: Option<String>,
}
