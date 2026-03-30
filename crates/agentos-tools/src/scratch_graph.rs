use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_scratch::ScratchpadStore;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;

pub struct ScratchGraphTool {
    store: Arc<ScratchpadStore>,
}

impl ScratchGraphTool {
    pub fn new(store: Arc<ScratchpadStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for ScratchGraphTool {
    fn name(&self) -> &str {
        "scratch-graph"
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
                    "scratch-graph requires 'title' field (string)".into(),
                )
            })?;

        let depth = payload
            .get("depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(2)
            .min(5) as usize;

        let max_pages = payload
            .get("max_pages")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(50) as usize;

        let cross_agent = payload
            .get("cross_agent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Default 512 KB byte budget; max_pages (≤50) × 64 KB/page ≈ 3.2 MB theoretical max
        let max_bytes = payload
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(512 * 1024)
            .min(3 * 1024 * 1024) as usize; // Cap at 3 MB

        let agent_id = context.agent_id.to_string();

        let walker = agentos_scratch::GraphWalker::new(&self.store);

        let subgraph = if cross_agent {
            // Build the set of agents we have cross-agent read permission for.
            // We check permissions for all agents whose scratch.cross:<id> is granted.
            let allowed_agents = build_allowed_agents(&context);

            walker
                .subgraph_cross_agent(
                    &agent_id,
                    title,
                    depth,
                    max_pages,
                    max_bytes,
                    &allowed_agents,
                )
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "scratch-graph".to_string(),
                    reason: format!("Cross-agent graph traversal failed: {}", e),
                })?
        } else {
            walker
                .subgraph(&agent_id, title, depth, max_pages, max_bytes)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "scratch-graph".to_string(),
                    reason: format!("Graph traversal failed: {}", e),
                })?
        };

        // Build response with nodes and edges
        let nodes: Vec<serde_json::Value> = subgraph
            .pages
            .iter()
            .enumerate()
            .map(|(i, page)| {
                serde_json::json!({
                    "title": page.title,
                    "agent_id": page.agent_id,
                    "depth": i, // BFS order approximates depth
                })
            })
            .collect();

        // Deduplicate edges for the response
        let mut edges_seen = std::collections::HashSet::new();
        let edges: Vec<serde_json::Value> = subgraph
            .edges
            .iter()
            .filter(|edge| edges_seen.insert((edge.0.clone(), edge.1.clone())))
            .map(|(from, to)| {
                serde_json::json!({
                    "from": from,
                    "to": to,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "center": title,
            "depth": depth,
            "cross_agent": cross_agent,
            "node_count": nodes.len(),
            "edge_count": edges.len(),
            "total_bytes": subgraph.total_bytes,
            "nodes": nodes,
            "edges": edges,
        }))
    }
}

/// Extract the set of agent IDs the calling agent has cross-agent scratchpad
/// read permission for, by inspecting PermissionSet entries that match the
/// `scratch.cross:` prefix.
fn build_allowed_agents(context: &ToolExecutionContext) -> HashSet<String> {
    let mut allowed = HashSet::new();
    for entry in context.permissions.entries() {
        if entry.read {
            if let Some(suffix) = entry.resource.strip_prefix("scratch.cross:") {
                if suffix.is_empty() {
                    // Bare "scratch.cross:" grant = wildcard. Insert "*" sentinel;
                    // the graph walker recognizes "*" to allow all agents.
                    allowed.insert("*".to_string());
                } else {
                    allowed.insert(suffix.to_string());
                }
            }
        }
    }
    allowed
}
