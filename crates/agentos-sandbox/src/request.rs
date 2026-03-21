use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxExecRequest {
    pub tool_name: String,
    pub payload: serde_json::Value,
    pub data_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_weight: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskID>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentID>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<TraceID>,
    pub permissions: PermissionSet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_paths: Option<Vec<PathBuf>>,
}
