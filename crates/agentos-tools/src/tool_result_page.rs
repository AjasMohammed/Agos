use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_scratch::{ScratchError, ScratchpadStore};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

/// Retrieve a paged section of a previously truncated tool result.
///
/// When `tool_executor` truncates a large tool output, it writes the original
/// content to a scratchpad page titled `tool-overflow:<tool_name>:<page_id>`.
/// The truncated text returned to the agent includes a `page_id=...` hint;
/// agents call this tool with that page_id to recover the full content.
pub struct ToolResultPageTool {
    store: Arc<ScratchpadStore>,
}

impl ToolResultPageTool {
    pub fn new(store: Arc<ScratchpadStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for ToolResultPageTool {
    fn name(&self) -> &str {
        "tool-result-page"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("scratchpad".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("scratchpad", PermissionOp::Read) {
            return Err(AgentOSError::PermissionDenied {
                resource: "scratchpad".to_string(),
                operation: format!("{:?}", PermissionOp::Read),
            });
        }

        let page_id = payload
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "tool-result-page requires 'page_id' field (string)".into(),
                )
            })?;

        if page_id.contains('/') || page_id.contains("..") {
            return Err(AgentOSError::SchemaValidation(
                "tool-result-page rejects page_id containing '/' or '..'".into(),
            ));
        }

        let offset = payload.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let length = payload
            .get("length")
            .and_then(|v| v.as_u64())
            .unwrap_or(4000)
            .min(8000) as usize;

        let agent_id = context.agent_id.to_string();
        let pages = match self.store.list_pages(&agent_id).await {
            Ok(pages) => pages,
            Err(e) => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "tool-result-page".into(),
                    reason: format!("List failed: {}", e),
                });
            }
        };

        let matching = pages
            .iter()
            .find(|p| p.title.contains(page_id))
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: "tool-result-page".into(),
                reason: format!(
                    "No overflow page found for page_id '{}'. Page may have been pruned (overflow pages expire after 24h).",
                    page_id
                ),
            })?;

        let page = match self.store.read_page(&agent_id, &matching.title).await {
            Ok(page) => page,
            Err(ScratchError::PageNotFound { .. }) => {
                return Ok(serde_json::json!({
                    "found": false,
                    "page_id": page_id,
                    "message": format!("Overflow page '{}' not found", page_id),
                }));
            }
            Err(e) => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "tool-result-page".into(),
                    reason: format!("Read failed: {}", e),
                });
            }
        };

        let total_chars = page.content.chars().count();
        let slice: String = page.content.chars().skip(offset).take(length).collect();
        let returned_chars = slice.chars().count();
        let next_offset = if offset + returned_chars < total_chars {
            Some(offset + returned_chars)
        } else {
            None
        };

        Ok(serde_json::json!({
            "found": true,
            "page_id": page_id,
            "offset": offset,
            "length": returned_chars,
            "total_chars": total_chars,
            "content": slice,
            "next_offset": next_offset,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;
    use tempfile::TempDir;

    fn make_ctx(agent_id: AgentID, with_perm: bool) -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        if with_perm {
            permissions.grant("scratchpad".to_string(), true, true, false, None);
        }
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
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
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
        }
    }

    async fn make_store_with_overflow(
        tmp: &TempDir,
        agent_id: &str,
        content: &str,
    ) -> (Arc<ScratchpadStore>, String) {
        let store = Arc::new(
            ScratchpadStore::new(&tmp.path().join("scratch.db"))
                .expect("open scratchpad in tempdir"),
        );
        let page_id = format!("overflow-{}", uuid::Uuid::new_v4());
        let title = format!("tool-overflow:test-tool:{}", page_id);
        store
            .write_page(agent_id, &title, content, &["tool-overflow".to_string()])
            .await
            .expect("write overflow page");
        (store, page_id)
    }

    #[tokio::test]
    async fn rejects_without_scratchpad_permission() {
        let tmp = TempDir::new().unwrap();
        let agent = AgentID::new();
        let (store, page_id) =
            make_store_with_overflow(&tmp, &agent.to_string(), "x".repeat(100).as_str()).await;
        let tool = ToolResultPageTool::new(store);
        let res = tool
            .execute(
                serde_json::json!({"page_id": page_id}),
                make_ctx(agent, false),
            )
            .await;
        assert!(matches!(res, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[tokio::test]
    async fn rejects_path_traversal_in_page_id() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(ScratchpadStore::new(&tmp.path().join("scratch.db")).unwrap());
        let tool = ToolResultPageTool::new(store);
        let res = tool
            .execute(
                serde_json::json!({"page_id": "../etc/passwd"}),
                make_ctx(AgentID::new(), true),
            )
            .await;
        assert!(matches!(res, Err(AgentOSError::SchemaValidation(_))));

        let res2 = tool
            .execute(
                serde_json::json!({"page_id": "foo/bar"}),
                make_ctx(AgentID::new(), true),
            )
            .await;
        assert!(matches!(res2, Err(AgentOSError::SchemaValidation(_))));
    }

    #[tokio::test]
    async fn returns_offset_and_pagination_metadata() {
        let tmp = TempDir::new().unwrap();
        let agent = AgentID::new();
        let content: String = (0..10_000)
            .map(|i| ((i % 26) as u8 + b'a') as char)
            .collect();
        let (store, page_id) = make_store_with_overflow(&tmp, &agent.to_string(), &content).await;
        let tool = ToolResultPageTool::new(store);

        let result = tool
            .execute(
                serde_json::json!({"page_id": page_id, "offset": 0, "length": 100}),
                make_ctx(agent.clone(), true),
            )
            .await
            .expect("first page should succeed");
        assert_eq!(result["found"], serde_json::Value::Bool(true));
        assert_eq!(result["offset"], serde_json::json!(0));
        assert_eq!(result["length"], serde_json::json!(100));
        assert_eq!(result["next_offset"], serde_json::json!(100));
        assert_eq!(result["content"].as_str().unwrap().chars().count(), 100);

        let last = tool
            .execute(
                serde_json::json!({"page_id": page_id, "offset": 9_900, "length": 200}),
                make_ctx(agent, true),
            )
            .await
            .expect("last page");
        assert_eq!(last["next_offset"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn missing_page_id_is_schema_validation_error() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(ScratchpadStore::new(&tmp.path().join("scratch.db")).unwrap());
        let tool = ToolResultPageTool::new(store);
        let res = tool
            .execute(serde_json::json!({}), make_ctx(AgentID::new(), true))
            .await;
        assert!(matches!(res, Err(AgentOSError::SchemaValidation(_))));
    }

    #[tokio::test]
    async fn unknown_page_returns_tool_execution_failed() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(ScratchpadStore::new(&tmp.path().join("scratch.db")).unwrap());
        let tool = ToolResultPageTool::new(store);
        let res = tool
            .execute(
                serde_json::json!({"page_id": "overflow-does-not-exist"}),
                make_ctx(AgentID::new(), true),
            )
            .await;
        assert!(matches!(res, Err(AgentOSError::ToolExecutionFailed { .. })));
    }
}
