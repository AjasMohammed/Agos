use crate::capability::PermissionSet;
use crate::ids::*;
use serde::{Deserialize, Serialize};

/// Profile of a connected LLM agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: AgentID,
    pub name: String,
    pub provider: LLMProvider,
    pub model: String,
    pub status: AgentStatus,
    /// Agent's explicitly granted custom permissions
    pub permissions: PermissionSet,
    /// Roles assigned to this agent
    #[serde(default)]
    pub roles: Vec<String>,
    pub current_task: Option<TaskID>,
    pub description: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_active: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LLMProvider {
    Ollama,
    OpenAI,
    Anthropic,
    Gemini,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Online,
    Idle,
    Busy,
    Offline,
}
