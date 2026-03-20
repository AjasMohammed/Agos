use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::{EpisodicStore, SemanticStore};
use agentos_types::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use uuid::Uuid;

pub struct MemorySearch {
    semantic: Arc<SemanticStore>,
    episodic: Arc<EpisodicStore>,
}

impl MemorySearch {
    pub fn new(semantic: Arc<SemanticStore>, episodic: Arc<EpisodicStore>) -> Self {
        Self { semantic, episodic }
    }
}

#[async_trait]
impl AgentTool for MemorySearch {
    fn name(&self) -> &str {
        "memory-search"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Scope-aware checks are enforced inside execute():
        // - semantic -> memory.semantic:r
        // - episodic global/cross-task -> memory.episodic:r
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("memory-search requires 'query' field".into())
            })?;

        const MAX_TOP_K: usize = 100;
        let top_k = payload
            .get("top_k")
            .or_else(|| payload.get("limit"))
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let top_k = top_k.min(MAX_TOP_K);

        let scope = payload
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("semantic");

        if scope == "episodic" {
            let global = payload
                .get("global")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let task_filter = payload
                .get("task_id")
                .and_then(|v| v.as_str())
                .map(|s| {
                    Uuid::parse_str(s).map(TaskID::from_uuid).map_err(|_| {
                        AgentOSError::SchemaValidation(
                            "memory-search payload.task_id must be a valid UUID".to_string(),
                        )
                    })
                })
                .transpose()?;

            let results = if global {
                if !context
                    .permissions
                    .check("memory.episodic", PermissionOp::Read)
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "memory.episodic".to_string(),
                        operation: format!("{:?}", PermissionOp::Read),
                    });
                }

                let since = payload
                    .get("since")
                    .and_then(|v| v.as_str())
                    .map(|s| {
                        DateTime::parse_from_rfc3339(s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .map_err(|_| {
                                AgentOSError::SchemaValidation(
                                    "memory-search payload.since must be RFC3339".to_string(),
                                )
                            })
                    })
                    .transpose()?;

                let agent_filter = payload
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .map(|s| {
                        Uuid::parse_str(s).map(AgentID::from_uuid).map_err(|_| {
                            AgentOSError::SchemaValidation(
                                "memory-search payload.agent_id must be a valid UUID".to_string(),
                            )
                        })
                    })
                    .transpose()?;

                let mut rows = self
                    .episodic
                    .recall_global(query, agent_filter.as_ref(), since, top_k)
                    .await
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "memory-search".into(),
                        reason: format!("Episodic global search failed: {}", e),
                    })?;

                if let Some(task_id) = task_filter {
                    rows.retain(|r| r.task_id == task_id);
                }
                rows
            } else if let Some(task_id) = task_filter {
                if task_id == context.task_id {
                    self.episodic
                        .recall_task(&task_id, &context.agent_id, query, top_k)
                        .await
                        .map_err(|e| AgentOSError::ToolExecutionFailed {
                            tool_name: "memory-search".into(),
                            reason: format!("Episodic task search failed: {}", e),
                        })?
                } else {
                    if !context
                        .permissions
                        .check("memory.episodic", PermissionOp::Read)
                    {
                        return Err(AgentOSError::PermissionDenied {
                            resource: "memory.episodic".to_string(),
                            operation: format!("{:?}", PermissionOp::Read),
                        });
                    }

                    self.episodic
                        .recall_task_with_permission(&task_id, query, top_k)
                        .await
                        .map_err(|e| AgentOSError::ToolExecutionFailed {
                            tool_name: "memory-search".into(),
                            reason: format!("Episodic cross-task search failed: {}", e),
                        })?
                }
            } else {
                self.episodic
                    .recall_task(&context.task_id, &context.agent_id, query, top_k)
                    .await
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "memory-search".into(),
                        reason: format!("Episodic task search failed: {}", e),
                    })?
            };

            let rows: Vec<serde_json::Value> = results
                .into_iter()
                .map(|ep| {
                    serde_json::json!({
                        "id": ep.id,
                        "task_id": ep.task_id.to_string(),
                        "agent_id": ep.agent_id.to_string(),
                        "content": ep.content,
                        "summary": ep.summary,
                        "entry_type": ep.entry_type.as_str(),
                        "timestamp": ep.timestamp.to_rfc3339(),
                        "scope": "episodic",
                    })
                })
                .collect();

            let count = rows.len();
            Ok(serde_json::json!({
                "query": query,
                "results": rows,
                "count": count,
            }))
        } else {
            if !context
                .permissions
                .check("memory.semantic", PermissionOp::Read)
            {
                return Err(AgentOSError::PermissionDenied {
                    resource: "memory.semantic".to_string(),
                    operation: format!("{:?}", PermissionOp::Read),
                });
            }

            let min_score = payload
                .get("min_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3) as f32;

            let results = self
                .semantic
                .search(query, Some(&context.agent_id), top_k, min_score)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-search".into(),
                    reason: format!("Semantic search failed: {}", e),
                })?;

            let rows: Vec<serde_json::Value> = results
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.entry.id,
                        "key": r.entry.key,
                        "content": r.chunk.content,
                        "full_content": r.entry.full_content,
                        "tags": r.entry.tags,
                        "score": r.semantic_score,
                        "semantic_score": r.semantic_score,
                        "fts_score": r.fts_score,
                        "rrf_score": r.rrf_score,
                        "created_at": r.entry.created_at.to_rfc3339(),
                        "scope": "semantic",
                    })
                })
                .collect();

            let count = rows.len();
            Ok(serde_json::json!({
                "query": query,
                "results": rows,
                "count": count,
            }))
        }
    }
}
