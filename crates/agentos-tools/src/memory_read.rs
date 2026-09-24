use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::{EpisodicStore, SemanticStore};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct MemoryRead {
    semantic: Arc<SemanticStore>,
    episodic: Arc<EpisodicStore>,
}

impl MemoryRead {
    pub fn new(semantic: Arc<SemanticStore>, episodic: Arc<EpisodicStore>) -> Self {
        Self { semantic, episodic }
    }
}

#[async_trait]
impl AgentTool for MemoryRead {
    fn name(&self) -> &str {
        "memory-read"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Coarse gate: agents need at least one memory read permission.
        // Scope-specific checks (memory.semantic vs memory.episodic) are
        // enforced inside execute().
        vec![("memory.read".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let scope = payload
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("semantic");

        match scope {
            "episodic" => {
                if !context
                    .permissions
                    .check("memory.episodic", PermissionOp::Read)
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "memory.episodic".to_string(),
                        operation: format!("{:?}", PermissionOp::Read),
                    });
                }

                let id = payload.get("id").and_then(|v| v.as_i64()).ok_or_else(|| {
                    AgentOSError::SchemaValidation(
                        "memory-read with scope='episodic' requires integer 'id' field".into(),
                    )
                })?;

                let entry = self.episodic.get_by_id(id).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "memory-read".into(),
                        reason: format!("Episodic read failed: {}", e),
                    }
                })?;

                match entry {
                    Some(e) => {
                        // Enforce agent-scoping: agents can only read their own episodic entries
                        if e.agent_id != context.agent_id {
                            return Ok(serde_json::json!({
                                "found": false,
                                "scope": "episodic",
                                "id": id,
                                "message": format!("No episodic entry found with id {}", id),
                            }));
                        }
                        Ok(serde_json::json!({
                            "found": true,
                            "scope": "episodic",
                            "id": e.id,
                            "task_id": e.task_id.as_uuid().to_string(),
                            "agent_id": e.agent_id.as_uuid().to_string(),
                            "entry_type": format!("{:?}", e.entry_type),
                            "content": e.content,
                            "summary": e.summary,
                            "metadata": e.metadata,
                            "timestamp": e.timestamp.to_rfc3339(),
                        }))
                    }
                    None => Ok(serde_json::json!({
                        "found": false,
                        "scope": "episodic",
                        "id": id,
                        "message": format!("No episodic entry found with id {}", id),
                    })),
                }
            }
            "semantic" => {
                if !context
                    .permissions
                    .check("memory.semantic", PermissionOp::Read)
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "memory.semantic".to_string(),
                        operation: format!("{:?}", PermissionOp::Read),
                    });
                }

                // Agent-scoped: a bare `get_by_key` is a global lookup, so any
                // agent that guessed a key could read another agent's private
                // fact (the episodic branch above already checks ownership).
                // ponytail: strict `agent_id = ?` also hides operator-imported
                // rows that carry no agent (still reachable via `memory-search`,
                // whose predicate is `agent_id IS NULL OR agent_id = ?`); widen
                // this lookup the same way if such shared rows ever matter.
                //
                // `id` (the UUID `memory-write`/`memory-search` return) wins
                // over `key`: the UUID is unique, a key is not.
                let id = payload.get("id").and_then(|v| v.as_str());
                let key = payload.get("key").and_then(|v| v.as_str());
                let (entry, lookup) = match (id, key) {
                    (Some(id), _) => (
                        self.semantic
                            .get_by_id_scoped(id, Some(&context.agent_id))
                            .await,
                        format!("id '{}'", id),
                    ),
                    (None, Some(key)) => (
                        self.semantic
                            .get_by_key_scoped(key, Some(&context.agent_id))
                            .await,
                        format!("key '{}'", key),
                    ),
                    (None, None) => return Err(AgentOSError::SchemaValidation(
                        "memory-read with scope='semantic' requires 'id' (UUID string) or 'key'"
                            .into(),
                    )),
                };
                let entry = entry.map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "memory-read".into(),
                    reason: format!("Read failed: {}", e),
                })?;

                match entry {
                    Some(e) => Ok(serde_json::json!({
                        "found": true,
                        "scope": "semantic",
                        "id": e.id,
                        "key": e.key,
                        "content": e.full_content,
                        "tags": e.tags,
                        "created_at": e.created_at.to_rfc3339(),
                        "updated_at": e.updated_at.to_rfc3339(),
                    })),
                    None => Ok(serde_json::json!({
                        "found": false,
                        "scope": "semantic",
                        "id": id,
                        "key": key,
                        "message": format!("No semantic memory entry found for {}", lookup),
                    })),
                }
            }
            other => Err(AgentOSError::SchemaValidation(format!(
                "Unknown memory scope '{}'. Valid values: 'semantic', 'episodic'",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_memory::Embedder;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::path::Path;
    use tempfile::TempDir;

    fn ctx(data_dir: &Path, agent_id: AgentID) -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant("memory.read".to_string(), true, false, false, None);
        permissions.grant("memory.semantic".to_string(), true, false, false, None);
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

    #[tokio::test]
    async fn semantic_read_by_key_is_agent_scoped() {
        let dir = TempDir::new().unwrap();
        let semantic = Arc::new(
            SemanticStore::open_with_embedder(dir.path(), Arc::new(Embedder::noop())).unwrap(),
        );
        let episodic = Arc::new(EpisodicStore::open(dir.path()).unwrap());
        let (alice, bob) = (AgentID::new(), AgentID::new());
        semantic
            .write("api-token-note", "alice private fact", Some(&alice), &[])
            .await
            .unwrap();

        let tool = MemoryRead::new(semantic, episodic);
        let payload = serde_json::json!({"scope": "semantic", "key": "api-token-note"});

        // Owner still reads its own entry.
        let own = tool
            .execute(payload.clone(), ctx(dir.path(), alice))
            .await
            .unwrap();
        assert_eq!(own["found"], true);
        assert_eq!(own["content"], "alice private fact");

        // Another agent guessing the key gets nothing.
        let other = tool.execute(payload, ctx(dir.path(), bob)).await.unwrap();
        assert_eq!(other["found"], false);
    }

    #[tokio::test]
    async fn semantic_read_by_uuid_id() {
        let dir = TempDir::new().unwrap();
        let semantic = Arc::new(
            SemanticStore::open_with_embedder(dir.path(), Arc::new(Embedder::noop())).unwrap(),
        );
        let episodic = Arc::new(EpisodicStore::open(dir.path()).unwrap());
        let (alice, bob) = (AgentID::new(), AgentID::new());
        let id = semantic
            .write("seed", "alice fact", Some(&alice), &["bridge"])
            .await
            .unwrap();

        let tool = MemoryRead::new(semantic, episodic);
        let payload = serde_json::json!({"scope": "semantic", "id": id});

        let own = tool
            .execute(payload.clone(), ctx(dir.path(), alice))
            .await
            .unwrap();
        assert_eq!(own["found"], true);
        assert_eq!(own["key"], "seed");

        let other = tool.execute(payload, ctx(dir.path(), bob)).await.unwrap();
        assert_eq!(other["found"], false);

        let neither = tool
            .execute(
                serde_json::json!({"scope": "semantic"}),
                ctx(dir.path(), alice),
            )
            .await;
        assert!(matches!(neither, Err(AgentOSError::SchemaValidation(_))));
    }
}
