use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::ProceduralStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ProcedureSearch {
    procedural: Arc<ProceduralStore>,
}

impl ProcedureSearch {
    pub fn new(procedural: Arc<ProceduralStore>) -> Self {
        Self { procedural }
    }
}

#[async_trait]
impl AgentTool for ProcedureSearch {
    fn name(&self) -> &str {
        "procedure-search"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.procedural".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("memory.procedural", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.procedural".to_string(),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("procedure-search requires 'query' field".into())
            })?;

        let top_k = payload
            .get("top_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(20) as usize;

        let min_score = payload
            .get("min_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32;

        // Scope to the caller: `None` means *no filter*, which let any agent
        // read every other agent's private procedures. The store's predicate
        // is `agent_id IS NULL OR agent_id = ?`, so globally-owned (shared)
        // procedures stay visible.
        let results = self
            .procedural
            .search(query, Some(&context.agent_id), top_k, min_score)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "procedure-search".into(),
                reason: format!("Search failed: {}", e),
            })?;

        let serialized: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.procedure.id,
                    "name": r.procedure.name,
                    "description": r.procedure.description,
                    "preconditions": r.procedure.preconditions,
                    "steps": r.procedure.steps.iter().map(|s| serde_json::json!({
                        "order": s.order,
                        "action": s.action,
                        "tool": s.tool,
                        "expected_outcome": s.expected_outcome,
                    })).collect::<Vec<_>>(),
                    "postconditions": r.procedure.postconditions,
                    "tags": r.procedure.tags,
                    "success_count": r.procedure.success_count,
                    "failure_count": r.procedure.failure_count,
                    "semantic_score": r.semantic_score,
                    "rrf_score": r.rrf_score,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "query": query,
            "count": serialized.len(),
            "results": serialized,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_memory::types::{Procedure, ProcedureStep};
    use agentos_memory::{Embedder, MemoryStatus};
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use chrono::Utc;
    use std::path::Path;
    use tempfile::TempDir;

    fn ctx(data_dir: &Path, agent_id: AgentID) -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant("memory.procedural".to_string(), true, false, false, None);
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            agent_id,
            trace_id: TraceID::new(),
            permissions,
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
            shared_dir: None,
        }
    }

    fn procedure(name: &str, owner: Option<AgentID>) -> Procedure {
        Procedure {
            id: String::new(),
            name: name.to_string(),
            description: "Deploy the ingest service safely".to_string(),
            preconditions: vec![],
            inputs: vec![],
            steps: vec![ProcedureStep {
                order: 0,
                action: "Call 'shell-exec'".to_string(),
                tool: Some("shell-exec".to_string()),
                expected_outcome: None,
                input: None,
                output_var: None,
            }],
            postconditions: vec![],
            success_count: 1,
            failure_count: 0,
            source_episodes: vec![],
            agent_id: owner,
            tags: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_used_at: None,
            use_count: 0,
            confidence: agentos_memory::types::default_confidence(),
            status: MemoryStatus::Active,
        }
    }

    #[tokio::test]
    async fn search_returns_own_and_global_procedures_but_not_another_agents() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(
            ProceduralStore::open_with_embedder(dir.path(), Arc::new(Embedder::noop())).unwrap(),
        );
        let (alice, bob) = (AgentID::new(), AgentID::new());
        store
            .store(&procedure("alice-deploy", Some(alice)))
            .await
            .unwrap();
        store
            .store(&procedure("bob-deploy", Some(bob)))
            .await
            .unwrap();
        store
            .store(&procedure("shared-deploy", None))
            .await
            .unwrap();

        let tool = ProcedureSearch::new(store);
        let out = tool
            .execute(
                serde_json::json!({"query": "deploy the ingest service", "top_k": 10}),
                ctx(dir.path(), alice),
            )
            .await
            .unwrap();

        let names: Vec<String> = out["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"alice-deploy".to_string()), "{:?}", names);
        assert!(names.contains(&"shared-deploy".to_string()), "{:?}", names);
        assert!(!names.contains(&"bob-deploy".to_string()), "{:?}", names);
    }
}
