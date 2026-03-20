use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::{EpisodicStore, ProceduralStore, SemanticStore};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct MemoryStats {
    semantic: Arc<SemanticStore>,
    episodic: Arc<EpisodicStore>,
    procedural: Arc<ProceduralStore>,
}

impl MemoryStats {
    pub fn new(
        semantic: Arc<SemanticStore>,
        episodic: Arc<EpisodicStore>,
        procedural: Arc<ProceduralStore>,
    ) -> Self {
        Self {
            semantic,
            episodic,
            procedural,
        }
    }
}

#[async_trait]
impl AgentTool for MemoryStats {
    fn name(&self) -> &str {
        "memory-stats"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Scope-aware checks are enforced inside execute():
        // each tier is checked individually and returns 0 if not permitted.
        vec![]
    }

    async fn execute(
        &self,
        _payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let agent_id = &context.agent_id;
        let semantic_accessible = context
            .permissions
            .check("memory.semantic", PermissionOp::Read);
        let episodic_accessible = context
            .permissions
            .check("memory.episodic", PermissionOp::Read);
        let procedural_accessible = context
            .permissions
            .check("memory.procedural", PermissionOp::Read);

        let semantic_count = if semantic_accessible {
            self.semantic.count(Some(agent_id)).await.map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-stats".into(),
                    reason: format!("Semantic count failed: {}", e),
                }
            })?
        } else {
            0
        };

        let episodic_count = if episodic_accessible {
            self.episodic.count(Some(agent_id)).await.map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-stats".into(),
                    reason: format!("Episodic count failed: {}", e),
                }
            })?
        } else {
            0
        };

        let procedural_count = if procedural_accessible {
            self.procedural.count(Some(agent_id)).await.map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-stats".into(),
                    reason: format!("Procedural count failed: {}", e),
                }
            })?
        } else {
            0
        };

        Ok(serde_json::json!({
            "agent_id": agent_id.to_string(),
            "tiers": {
                "semantic": {
                    "entries": semantic_count,
                    "accessible": semantic_accessible,
                    "description": "Long-term knowledge with embeddings"
                },
                "episodic": {
                    "entries": episodic_count,
                    "accessible": episodic_accessible,
                    "description": "Task-scoped event log"
                },
                "procedural": {
                    "entries": procedural_count,
                    "accessible": procedural_accessible,
                    "description": "Learned procedures and SOPs"
                }
            },
            "total_entries": semantic_count + episodic_count + procedural_count,
        }))
    }
}
