use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub struct MemorySearch {
    data_dir: PathBuf,
}

impl MemorySearch {
    pub fn new(data_dir: &Path) -> Self {
        let db_path = data_dir.join("semantic_memory.db");
        Self::init_db(&db_path).ok();
        Self { data_dir: data_dir.to_path_buf() }
    }

    fn init_db(path: &Path) -> Result<(), AgentOSError> {
        let conn = Connection::open(path).map_err(|e| {
            AgentOSError::ToolExecutionFailed { tool_name: "memory-search".into(), reason: format!("Failed to open semantic_memory db: {}", e) }
        })?;
        conn.execute_batch(
            "
            CREATE VIRTUAL TABLE IF NOT EXISTS memory USING fts5(
                content,
                source,
                tags,
                created_at
            );
        ",
        )
        .map_err(|e| {
            AgentOSError::ToolExecutionFailed { tool_name: "memory-search".into(), reason: format!("Failed to init semantic_memory fts5: {}", e) }
        })?;
        Ok(())
    }
}

#[async_trait]
impl AgentTool for MemorySearch {
    fn name(&self) -> &str {
        "memory-search"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("memory-search requires 'query' field".into())
            })?;

        let limit = payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let scope = payload.get("scope").and_then(|v| v.as_str()).unwrap_or("semantic").to_string();

        let data_dir = self.data_dir.clone();
        let query_owned = query.to_string();

        // Run SQLite query on a blocking thread (rusqlite is not async)
        let results = tokio::task::spawn_blocking(move || {
            let db_name = match scope.as_str() {
                "episodic" => "episodic_memory.db",
                _ => "semantic_memory.db",
            };
            let db_path = data_dir.join(db_name);

            let conn = Connection::open(&db_path).map_err(|e| {
                AgentOSError::ToolExecutionFailed { tool_name: "memory-search".into(), reason: format!("Failed to open memory db for search: {}", e) }
            })?;

            if scope == "episodic" {
                let mut stmt = conn.prepare(
                    "SELECT content, entry_type as source, '' as tags, timestamp as created_at
                     FROM episodes
                     WHERE content LIKE '%' || ?1 || '%'
                     ORDER BY timestamp DESC
                     LIMIT ?2"
                ).map_err(|e| AgentOSError::ToolExecutionFailed { tool_name: "memory-search".into(), reason: format!("Query prep failed: {}", e) })?;

                let rows: Vec<serde_json::Value> = stmt.query_map(rusqlite::params![&query_owned, limit], |row| {
                    Ok(serde_json::json!({
                        "content": row.get::<_, String>(0)?,
                        "source": row.get::<_, String>(1)?,
                        "tags": row.get::<_, String>(2)?,
                        "created_at": row.get::<_, String>(3)?,
                        "scope": "episodic",
                    }))
                }).map_err(|e| AgentOSError::ToolExecutionFailed { tool_name: "memory-search".into(), reason: e.to_string() })?.filter_map(|r| r.ok()).collect();
                Ok::<_, AgentOSError>(rows)
            } else {
                let mut stmt = conn.prepare(
                    "SELECT content, source, tags, created_at, bm25(memory) as rank
                     FROM memory
                     WHERE memory MATCH ?1
                     ORDER BY rank
                     LIMIT ?2"
                ).map_err(|e| AgentOSError::ToolExecutionFailed { tool_name: "memory-search".into(), reason: format!("Query prep failed: {}", e) })?;

                let rows: Vec<serde_json::Value> = stmt.query_map(rusqlite::params![&query_owned, limit], |row| {
                    Ok(serde_json::json!({
                        "content": row.get::<_, String>(0)?,
                        "source": row.get::<_, String>(1)?,
                        "tags": row.get::<_, String>(2)?,
                        "created_at": row.get::<_, String>(3)?,
                        "score": row.get::<_, f64>(4)?,
                        "scope": "semantic",
                    }))
                }).map_err(|e| AgentOSError::ToolExecutionFailed { tool_name: "memory-search".into(), reason: e.to_string() })?.filter_map(|r| r.ok()).collect();
                Ok::<_, AgentOSError>(rows)
            }
        })
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "memory-search".into(),
            reason: format!("Task join error: {}", e),
        })??;

        Ok(serde_json::json!({
            "query": query,
            "results": results,
            "count": results.len(),
        }))
    }
}
