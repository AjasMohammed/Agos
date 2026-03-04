use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub struct MemoryWrite {
    data_dir: PathBuf,
}

impl MemoryWrite {
    pub fn new(data_dir: &Path) -> Self {
        Self { data_dir: data_dir.to_path_buf() }
    }
}

#[async_trait]
impl AgentTool for MemoryWrite {
    fn name(&self) -> &str {
        "memory-write"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("memory-write requires 'content' field".into())
            })?;

        let source = payload
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");

        let tags = payload.get("tags").and_then(|v| v.as_str()).unwrap_or("");
        let scope = payload.get("scope").and_then(|v| v.as_str()).unwrap_or("semantic").to_string();

        let data_dir = self.data_dir.clone();
        let content = content.to_string();
        let source = source.to_string();
        let tags = tags.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        // Grab task and trace IDs for episodic DB
        let task_id_str = _context.task_id.as_uuid().to_string();
        let trace_id_str = _context.trace_id.as_uuid().to_string();

        tokio::task::spawn_blocking(move || {
            let db_name = match scope.as_str() {
                "episodic" => "episodic_memory.db",
                _ => "semantic_memory.db",
            };
            let db_path = data_dir.join(db_name);

            let conn = Connection::open(&db_path).map_err(|e| {
                AgentOSError::ToolExecutionFailed { tool_name: "memory-write".into(), reason: format!("Failed to open memory db for write: {}", e) }
            })?;

            if scope == "episodic" {
                conn.execute(
                    "INSERT INTO episodes (task_id, agent_id, entry_type, content, metadata, timestamp, trace_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![&task_id_str, "00000000-0000-0000-0000-000000000000", "tool_write", &content, "{}", &now, &trace_id_str],
                ).map_err(|e| AgentOSError::ToolExecutionFailed { tool_name: "memory-write".into(), reason: format!("Insert failed: {}", e) })?;
            } else {
                conn.execute(
                    "INSERT INTO memory (content, source, tags, created_at) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![&content, &source, &tags, &now],
                ).map_err(|e| AgentOSError::ToolExecutionFailed { tool_name: "memory-write".into(), reason: format!("Insert failed: {}", e) })?;
            }
            Ok::<_, AgentOSError>(())
        })
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "memory-write".into(),
            reason: format!("Task join error: {}", e),
        })??;

        Ok(serde_json::json!({
            "success": true,
            "message": "Memory entry stored successfully",
        }))
    }
}
