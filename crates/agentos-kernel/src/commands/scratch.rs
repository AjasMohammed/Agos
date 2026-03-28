use crate::Kernel;
use agentos_bus::message::KernelResponse;
use agentos_scratch::GraphWalker;
use serde_json::json;
use tracing::error;

impl Kernel {
    pub(crate) async fn cmd_scratch_list_pages(&self, agent_id: String) -> KernelResponse {
        match self.scratchpad_store.list_pages(&agent_id).await {
            Ok(pages) => {
                let page_summaries: Vec<_> = pages
                    .iter()
                    .map(|p| {
                        json!({
                            "title": p.title,
                            "tags": p.tags,
                            "updated_at": p.updated_at.to_rfc3339(),
                        })
                    })
                    .collect();

                KernelResponse::Success {
                    data: Some(json!({
                        "agent_id": agent_id,
                        "count": page_summaries.len(),
                        "pages": page_summaries,
                    })),
                }
            }
            Err(e) => {
                error!("Failed to list scratchpad pages: {}", e);
                KernelResponse::Error {
                    message: format!("Failed to list scratchpad pages: {}", e),
                }
            }
        }
    }

    pub(crate) async fn cmd_scratch_read_page(
        &self,
        agent_id: String,
        title: String,
    ) -> KernelResponse {
        match self.scratchpad_store.read_page(&agent_id, &title).await {
            Ok(page) => KernelResponse::Success {
                data: Some(json!({
                    "found": true,
                    "page_id": page.id,
                    "agent_id": page.agent_id,
                    "title": page.title,
                    "content": page.content,
                    "tags": page.tags,
                    "metadata": page.metadata,
                    "created_at": page.created_at.to_rfc3339(),
                    "updated_at": page.updated_at.to_rfc3339(),
                })),
            },
            Err(e) => {
                error!("Failed to read scratchpad page '{}': {}", title, e);
                KernelResponse::Error {
                    message: format!("Failed to read scratchpad page '{}': {}", title, e),
                }
            }
        }
    }

    pub(crate) async fn cmd_scratch_delete_page(
        &self,
        agent_id: String,
        title: String,
    ) -> KernelResponse {
        match self.scratchpad_store.delete_page(&agent_id, &title).await {
            Ok(_) => KernelResponse::Success {
                data: Some(json!({
                    "deleted": true,
                    "title": title,
                })),
            },
            Err(e) => {
                error!("Failed to delete scratchpad page '{}': {}", title, e);
                KernelResponse::Error {
                    message: format!("Failed to delete scratchpad page '{}': {}", title, e),
                }
            }
        }
    }

    pub(crate) async fn cmd_scratch_graph_page(
        &self,
        agent_id: String,
        title: String,
        depth: usize,
    ) -> KernelResponse {
        // Clamp depth to reasonable max (5 levels)
        let depth = depth.min(5);

        let walker = GraphWalker::new(&self.scratchpad_store);

        match walker
            .subgraph(&agent_id, &title, depth, 50, 512 * 1024)
            .await
        {
            Ok(subgraph) => {
                // Zip pages with their depths for the nodes output
                let nodes: Vec<_> = subgraph
                    .pages
                    .iter()
                    .zip(subgraph.depths.iter())
                    .map(|(p, d)| {
                        json!({
                            "title": p.title,
                            "agent_id": p.agent_id,
                            "depth": d,
                        })
                    })
                    .collect();

                let edges: Vec<_> = subgraph
                    .edges
                    .iter()
                    .map(|(from, to)| {
                        json!({
                            "from": from,
                            "to": to,
                        })
                    })
                    .collect();

                KernelResponse::Success {
                    data: Some(json!({
                        "center": title,
                        "agent_id": agent_id,
                        "depth": depth,
                        "node_count": subgraph.pages.len(),
                        "edge_count": subgraph.edges.len(),
                        "total_bytes": subgraph.total_bytes,
                        "nodes": nodes,
                        "edges": edges,
                    })),
                }
            }
            Err(e) => {
                error!("Failed to walk scratchpad graph for '{}': {}", title, e);
                KernelResponse::Error {
                    message: format!("Failed to walk scratchpad graph for '{}': {}", title, e),
                }
            }
        }
    }
}
