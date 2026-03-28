use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_scratch::ScratchpadStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::sync::Arc;

pub struct ScratchLinksTool {
    store: Arc<ScratchpadStore>,
}

impl ScratchLinksTool {
    pub fn new(store: Arc<ScratchpadStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for ScratchLinksTool {
    fn name(&self) -> &str {
        "scratch-links"
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

        let title = payload
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "scratch-links requires 'title' field (string)".into(),
                )
            })?;

        let direction = payload
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("both");

        let agent_id = context.agent_id.to_string();

        match direction {
            "inbound" => {
                let backlinks = self
                    .store
                    .get_backlinks(&agent_id, title)
                    .await
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "scratch-links".into(),
                        reason: format!("Backlinks query failed: {}", e),
                    })?;

                let items: Vec<serde_json::Value> = backlinks
                    .into_iter()
                    .map(|p| {
                        serde_json::json!({
                            "title": p.title,
                            "tags": p.tags,
                            "updated_at": p.updated_at.to_rfc3339(),
                        })
                    })
                    .collect();

                Ok(serde_json::json!({
                    "title": title,
                    "direction": "inbound",
                    "backlinks": items,
                }))
            }
            "outbound" => {
                let outlinks = self
                    .store
                    .get_outlinks(&agent_id, title)
                    .await
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "scratch-links".into(),
                        reason: format!("Outlinks query failed: {}", e),
                    })?;

                Ok(serde_json::json!({
                    "title": title,
                    "direction": "outbound",
                    "outlinks": outlinks,
                }))
            }
            "both" => {
                let info = self
                    .store
                    .get_all_links(&agent_id, title)
                    .await
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "scratch-links".into(),
                        reason: format!("Links query failed: {}", e),
                    })?;

                let backlinks: Vec<serde_json::Value> = info
                    .backlinks
                    .into_iter()
                    .map(|p| {
                        serde_json::json!({
                            "title": p.title,
                            "tags": p.tags,
                            "updated_at": p.updated_at.to_rfc3339(),
                        })
                    })
                    .collect();

                Ok(serde_json::json!({
                    "title": title,
                    "direction": "both",
                    "backlinks": backlinks,
                    "outlinks": info.outlinks,
                    "unresolved": info.unresolved,
                }))
            }
            other => Err(AgentOSError::SchemaValidation(format!(
                "Invalid direction '{}'. Valid: inbound, outbound, both",
                other
            ))),
        }
    }
}
