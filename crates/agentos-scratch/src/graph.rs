use crate::error::ScratchError;
use crate::store::ScratchpadStore;
use crate::types::ScratchPage;
use std::collections::{HashSet, VecDeque};
use tracing::debug;

/// Result of a BFS subgraph traversal from a starting page.
#[derive(Debug, Clone)]
pub struct SubgraphResult {
    /// Pages collected in BFS order (closest to start first).
    pub pages: Vec<ScratchPage>,
    /// BFS hop distance from the start page for each corresponding page in `pages`.
    /// `depths[i]` is the hop count for `pages[i]` (0 = start page itself).
    pub depths: Vec<usize>,
    /// Directed edges discovered: (source_title, target_title).
    pub edges: Vec<(String, String)>,
    /// Total content bytes across all collected pages.
    pub total_bytes: usize,
}

/// BFS graph walker over the scratchpad link graph.
///
/// Traverses both outbound links (pages this page links to) and inbound links
/// (pages that link to this page) at each level, collecting pages up to
/// configurable depth, page count, and byte limits.
pub struct GraphWalker<'a> {
    store: &'a ScratchpadStore,
}

impl<'a> GraphWalker<'a> {
    pub fn new(store: &'a ScratchpadStore) -> Self {
        Self { store }
    }

    /// BFS traversal from a starting page, collecting pages up to depth/size limits.
    ///
    /// Traverses both outbound links (pages this page links to) and inbound links
    /// (pages that link to this page) at each level.
    ///
    /// Returns pages ordered by distance from start (closest first).
    pub async fn subgraph(
        &self,
        agent_id: &str,
        start_title: &str,
        max_depth: usize,
        max_pages: usize,
        max_bytes: usize,
    ) -> Result<SubgraphResult, ScratchError> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut result_pages: Vec<ScratchPage> = Vec::new();
        let mut result_depths: Vec<usize> = Vec::new();
        let mut result_edges: Vec<(String, String)> = Vec::new();
        let mut total_bytes: usize = 0;

        queue.push_back((start_title.to_string(), 0));
        visited.insert(start_title.to_string());

        while let Some((title, depth)) = queue.pop_front() {
            // Check page count limit
            if result_pages.len() >= max_pages {
                break;
            }

            // Check byte budget (but always include at least the start page)
            if total_bytes >= max_bytes && !result_pages.is_empty() {
                break;
            }

            // Fetch the page
            match self.store.read_page(agent_id, &title).await {
                Ok(page) => {
                    let page_size = page.content.len();
                    // Don't exceed byte budget (always include at least the start page)
                    if total_bytes + page_size > max_bytes && !result_pages.is_empty() {
                        break;
                    }
                    total_bytes += page_size;
                    result_pages.push(page);
                    result_depths.push(depth);
                }
                Err(ScratchError::PageNotFound { .. }) => continue, // Unresolved link — skip
                Err(e) => return Err(e),
            }

            // Don't expand beyond max depth
            if depth >= max_depth {
                continue;
            }

            // Get outbound links (titles this page links to)
            let outlinks = self.store.get_outlinks(agent_id, &title).await?;
            for target in &outlinks {
                result_edges.push((title.clone(), target.clone()));
                if !visited.contains(target) {
                    visited.insert(target.clone());
                    queue.push_back((target.clone(), depth + 1));
                }
            }

            // Get inbound links (pages that link to this page)
            let backlinks = self.store.get_backlinks(agent_id, &title).await?;
            for bl in &backlinks {
                result_edges.push((bl.title.clone(), title.clone()));
                if !visited.contains(&bl.title) {
                    visited.insert(bl.title.clone());
                    queue.push_back((bl.title.clone(), depth + 1));
                }
            }
        }

        Ok(SubgraphResult {
            pages: result_pages,
            depths: result_depths,
            edges: result_edges,
            total_bytes,
        })
    }

    /// BFS traversal that follows cross-agent links when permitted.
    ///
    /// Behaves like `subgraph()` but when a cross-agent outlink is encountered,
    /// checks whether the target agent is in `allowed_agents`. If yes, the BFS
    /// continues into that agent's scratchpad. If no, the edge is skipped silently.
    ///
    /// Cross-agent edges are recorded as `"@agent_id/title"` in the edges vec
    /// so callers can distinguish them from same-agent edges.
    pub async fn subgraph_cross_agent(
        &self,
        agent_id: &str,
        start_title: &str,
        max_depth: usize,
        max_pages: usize,
        max_bytes: usize,
        allowed_agents: &HashSet<String>,
    ) -> Result<SubgraphResult, ScratchError> {
        let mut visited: HashSet<(String, String)> = HashSet::new(); // (agent_id, title)
        let mut queue: VecDeque<(String, String, usize)> = VecDeque::new(); // (agent_id, title, depth)
        let mut result_pages: Vec<ScratchPage> = Vec::new();
        let mut result_depths: Vec<usize> = Vec::new();
        let mut result_edges: Vec<(String, String)> = Vec::new();
        let mut total_bytes: usize = 0;

        queue.push_back((agent_id.to_string(), start_title.to_string(), 0));
        visited.insert((agent_id.to_string(), start_title.to_string()));

        while let Some((current_agent, title, depth)) = queue.pop_front() {
            if result_pages.len() >= max_pages {
                break;
            }
            if total_bytes >= max_bytes && !result_pages.is_empty() {
                break;
            }

            // Fetch the page from the appropriate agent's scratchpad
            match self.store.read_page(&current_agent, &title).await {
                Ok(page) => {
                    let page_size = page.content.len();
                    if total_bytes + page_size > max_bytes && !result_pages.is_empty() {
                        break;
                    }
                    total_bytes += page_size;
                    result_pages.push(page);
                    result_depths.push(depth);
                }
                Err(ScratchError::PageNotFound { .. }) => continue,
                Err(e) => return Err(e),
            }

            if depth >= max_depth {
                continue;
            }

            // Format a node label: same-agent pages use bare title, cross-agent use @agent/title
            let node_label = |agent: &str, t: &str| -> String {
                if agent == agent_id {
                    t.to_string()
                } else {
                    format!("@{agent}/{t}")
                }
            };

            let from_label = node_label(&current_agent, &title);

            // Get detailed outlinks (includes cross-agent info)
            let outlinks = self
                .store
                .get_outlinks_detailed(&current_agent, &title)
                .await?;

            for link in &outlinks {
                if link.is_cross_agent {
                    // Cross-agent link — only follow if the target agent is allowed
                    if let Some(target_agent) = &link.target_agent_id {
                        let to_label = format!("@{}/{}", target_agent, link.target_title);
                        result_edges.push((from_label.clone(), to_label));

                        // "*" is a wildcard sentinel meaning "all agents allowed"
                        if allowed_agents.contains("*")
                            || allowed_agents.contains(target_agent.as_str())
                        {
                            let key = (target_agent.clone(), link.target_title.clone());
                            if !visited.contains(&key) {
                                visited.insert(key);
                                queue.push_back((
                                    target_agent.clone(),
                                    link.target_title.clone(),
                                    depth + 1,
                                ));
                            }
                        } else {
                            debug!(
                                target_agent = %target_agent,
                                title = %link.target_title,
                                "Skipping cross-agent link: agent not in allowed set"
                            );
                        }
                    }
                } else {
                    // Same-agent link
                    let to_label = node_label(&current_agent, &link.target_title);
                    result_edges.push((from_label.clone(), to_label));

                    let key = (current_agent.clone(), link.target_title.clone());
                    if !visited.contains(&key) {
                        visited.insert(key);
                        queue.push_back((
                            current_agent.clone(),
                            link.target_title.clone(),
                            depth + 1,
                        ));
                    }
                }
            }

            // Get inbound links (same-agent only — backlinks are agent-scoped)
            let backlinks = self.store.get_backlinks(&current_agent, &title).await?;

            for bl in &backlinks {
                let bl_label = node_label(&current_agent, &bl.title);
                result_edges.push((bl_label, from_label.clone()));

                let key = (current_agent.clone(), bl.title.clone());
                if !visited.contains(&key) {
                    visited.insert(key);
                    queue.push_back((current_agent.clone(), bl.title.clone(), depth + 1));
                }
            }
        }

        Ok(SubgraphResult {
            pages: result_pages,
            depths: result_depths,
            edges: result_edges,
            total_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a store, write pages, and return the store.
    async fn setup_graph() -> ScratchpadStore {
        let store = ScratchpadStore::in_memory().unwrap();
        let agent = "agent-1";

        // Create a graph: A -> B -> C, A -> D, E -> A (backlink)
        store
            .write_page(agent, "A", "Links to [[B]] and [[D]]", &[])
            .await
            .unwrap();
        store
            .write_page(agent, "B", "Links to [[C]]", &[])
            .await
            .unwrap();
        store
            .write_page(agent, "C", "Leaf page, no links", &[])
            .await
            .unwrap();
        store
            .write_page(agent, "D", "Another leaf page", &[])
            .await
            .unwrap();
        store
            .write_page(agent, "E", "Links back to [[A]]", &[])
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn test_bfs_depth_0() {
        let store = setup_graph().await;
        let walker = GraphWalker::new(&store);
        let result = walker
            .subgraph("agent-1", "A", 0, 100, 100_000)
            .await
            .unwrap();

        // Depth 0: only the start page
        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.pages[0].title, "A");
        assert_eq!(result.depths, vec![0]);
        assert!(result.edges.is_empty());
    }

    #[tokio::test]
    async fn test_bfs_depth_1() {
        let store = setup_graph().await;
        let walker = GraphWalker::new(&store);
        let result = walker
            .subgraph("agent-1", "A", 1, 100, 100_000)
            .await
            .unwrap();

        // Depth 1: A + directly linked pages (B, D via outlinks; E via backlink)
        let titles: Vec<&str> = result.pages.iter().map(|p| p.title.as_str()).collect();
        assert!(titles.contains(&"A"));
        assert!(titles.contains(&"B"));
        assert!(titles.contains(&"D"));
        assert!(titles.contains(&"E"));
        // C is depth 2 (A->B->C), should NOT be included
        assert!(!titles.contains(&"C"));
    }

    #[tokio::test]
    async fn test_bfs_depth_2() {
        let store = setup_graph().await;
        let walker = GraphWalker::new(&store);
        let result = walker
            .subgraph("agent-1", "A", 2, 100, 100_000)
            .await
            .unwrap();

        // Depth 2: should include A, B, D, E (depth 1) and C (depth 2 via A->B->C)
        let titles: Vec<&str> = result.pages.iter().map(|p| p.title.as_str()).collect();
        assert!(titles.contains(&"A"));
        assert!(titles.contains(&"B"));
        assert!(titles.contains(&"C"));
        assert!(titles.contains(&"D"));
        assert!(titles.contains(&"E"));
        assert_eq!(titles.len(), 5);
        // Verify depths match pages
        assert_eq!(result.depths.len(), result.pages.len());
        // A is at depth 0
        let a_idx = titles.iter().position(|t| *t == "A").unwrap();
        assert_eq!(result.depths[a_idx], 0);
        // C is at depth 2 (A -> B -> C)
        let c_idx = titles.iter().position(|t| *t == "C").unwrap();
        assert_eq!(result.depths[c_idx], 2);
    }

    #[tokio::test]
    async fn test_bfs_max_pages() {
        let store = setup_graph().await;
        let walker = GraphWalker::new(&store);
        let result = walker
            .subgraph("agent-1", "A", 5, 3, 100_000)
            .await
            .unwrap();

        // Should stop after collecting 3 pages
        assert_eq!(result.pages.len(), 3);
        // First page is always the start
        assert_eq!(result.pages[0].title, "A");
    }

    #[tokio::test]
    async fn test_bfs_max_bytes() {
        let store = setup_graph().await;
        let walker = GraphWalker::new(&store);

        // Set a very small byte budget — only the start page should fit
        // Page A content is "Links to [[B]] and [[D]]" = 24 bytes
        let result = walker.subgraph("agent-1", "A", 5, 100, 25).await.unwrap();

        // Should include start page (always included) but stop before adding more
        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.pages[0].title, "A");
    }

    #[tokio::test]
    async fn test_bfs_cycle() {
        let store = ScratchpadStore::in_memory().unwrap();
        let agent = "agent-1";

        // Create a cycle: X -> Y -> Z -> X
        store
            .write_page(agent, "X", "Links to [[Y]]", &[])
            .await
            .unwrap();
        store
            .write_page(agent, "Y", "Links to [[Z]]", &[])
            .await
            .unwrap();
        store
            .write_page(agent, "Z", "Links to [[X]]", &[])
            .await
            .unwrap();

        let walker = GraphWalker::new(&store);
        let result = walker.subgraph(agent, "X", 10, 100, 100_000).await.unwrap();

        // Should visit all 3 pages without infinite loop
        assert_eq!(result.pages.len(), 3);
        let titles: Vec<&str> = result.pages.iter().map(|p| p.title.as_str()).collect();
        assert!(titles.contains(&"X"));
        assert!(titles.contains(&"Y"));
        assert!(titles.contains(&"Z"));
    }

    #[tokio::test]
    async fn test_bfs_unresolved_links() {
        let store = ScratchpadStore::in_memory().unwrap();
        let agent = "agent-1";

        // Page links to a non-existent page
        store
            .write_page(agent, "Exists", "Links to [[Ghost]]", &[])
            .await
            .unwrap();

        let walker = GraphWalker::new(&store);
        let result = walker
            .subgraph(agent, "Exists", 2, 100, 100_000)
            .await
            .unwrap();

        // Should gracefully skip Ghost, only include Exists
        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.pages[0].title, "Exists");
        // Edge is still recorded
        assert!(result
            .edges
            .iter()
            .any(|(from, to)| from == "Exists" && to == "Ghost"));
    }

    #[tokio::test]
    async fn test_bfs_includes_backlinks() {
        let store = ScratchpadStore::in_memory().unwrap();
        let agent = "agent-1";

        // B links to A, but we start from A
        store
            .write_page(agent, "A", "No outlinks here", &[])
            .await
            .unwrap();
        store
            .write_page(agent, "B", "Links to [[A]]", &[])
            .await
            .unwrap();

        let walker = GraphWalker::new(&store);
        let result = walker.subgraph(agent, "A", 1, 100, 100_000).await.unwrap();

        // B should be discovered via backlinks
        let titles: Vec<&str> = result.pages.iter().map(|p| p.title.as_str()).collect();
        assert!(titles.contains(&"A"));
        assert!(titles.contains(&"B"));
    }

    #[tokio::test]
    async fn test_bfs_nonexistent_start() {
        let store = ScratchpadStore::in_memory().unwrap();
        let walker = GraphWalker::new(&store);
        let result = walker
            .subgraph("agent-1", "NoSuchPage", 2, 100, 100_000)
            .await
            .unwrap();

        // Non-existent start page: gracefully returns empty
        assert!(result.pages.is_empty());
    }

    #[tokio::test]
    async fn test_subgraph_total_bytes_tracks_content() {
        let store = ScratchpadStore::in_memory().unwrap();
        let agent = "agent-1";

        store.write_page(agent, "P1", "Hello", &[]).await.unwrap();
        store
            .write_page(agent, "P2", "Links to [[P1]]", &[])
            .await
            .unwrap();

        let walker = GraphWalker::new(&store);
        let result = walker.subgraph(agent, "P1", 1, 100, 100_000).await.unwrap();

        let expected_bytes: usize = result.pages.iter().map(|p| p.content.len()).sum();
        assert_eq!(result.total_bytes, expected_bytes);
        assert!(result.total_bytes > 0);
    }

    // ─── Cross-agent graph traversal tests ───

    /// Set up a cross-agent scenario: agent-1 has pages linking to agent-2's pages.
    async fn setup_cross_agent_graph() -> ScratchpadStore {
        let store = ScratchpadStore::in_memory().unwrap();

        // Agent 1's pages
        store
            .write_page(
                "agent-1",
                "Research",
                "See [[@agent-2/Bug Report]] for details. Also [[Notes]].",
                &[],
            )
            .await
            .unwrap();
        store
            .write_page("agent-1", "Notes", "Local notes page", &[])
            .await
            .unwrap();

        // Agent 2's pages
        store
            .write_page(
                "agent-2",
                "Bug Report",
                "Found error in module X. See [[@agent-1/Research]].",
                &[],
            )
            .await
            .unwrap();
        store
            .write_page("agent-2", "Fix Plan", "Plan to fix module X", &[])
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn test_cross_agent_graph_follows_permitted_links() {
        let store = setup_cross_agent_graph().await;
        let walker = GraphWalker::new(&store);

        let mut allowed = HashSet::new();
        allowed.insert("agent-2".to_string());

        let result = walker
            .subgraph_cross_agent("agent-1", "Research", 2, 100, 100_000, &allowed)
            .await
            .unwrap();

        // Should include agent-1's Research & Notes, plus agent-2's Bug Report
        let titles: Vec<(&str, &str)> = result
            .pages
            .iter()
            .map(|p| (p.agent_id.as_str(), p.title.as_str()))
            .collect();
        assert!(titles.contains(&("agent-1", "Research")));
        assert!(titles.contains(&("agent-1", "Notes")));
        assert!(titles.contains(&("agent-2", "Bug Report")));

        // Cross-agent edge should be recorded with @agent_id/title format
        assert!(result
            .edges
            .iter()
            .any(|(from, to)| from == "Research" && to == "@agent-2/Bug Report"));
    }

    #[tokio::test]
    async fn test_cross_agent_graph_stops_at_boundary() {
        let store = setup_cross_agent_graph().await;
        let walker = GraphWalker::new(&store);

        // No agents allowed — cross-agent links should be skipped
        let allowed: HashSet<String> = HashSet::new();

        let result = walker
            .subgraph_cross_agent("agent-1", "Research", 2, 100, 100_000, &allowed)
            .await
            .unwrap();

        // Should only include agent-1's pages
        let agent_ids: HashSet<&str> = result.pages.iter().map(|p| p.agent_id.as_str()).collect();
        assert!(agent_ids.contains("agent-1"));
        assert!(!agent_ids.contains("agent-2"));

        // The cross-agent edge should still be recorded (as unresolved)
        assert!(result
            .edges
            .iter()
            .any(|(_, to)| to == "@agent-2/Bug Report"));
    }

    #[tokio::test]
    async fn test_cross_agent_graph_wildcard() {
        let store = setup_cross_agent_graph().await;
        let walker = GraphWalker::new(&store);

        // Wildcard: "*" allows all agents
        let mut allowed = HashSet::new();
        allowed.insert("*".to_string());

        let result = walker
            .subgraph_cross_agent("agent-1", "Research", 2, 100, 100_000, &allowed)
            .await
            .unwrap();

        // Should traverse into agent-2's pages
        let agent_ids: HashSet<&str> = result.pages.iter().map(|p| p.agent_id.as_str()).collect();
        assert!(agent_ids.contains("agent-1"));
        assert!(agent_ids.contains("agent-2"));
    }

    #[tokio::test]
    async fn test_cross_agent_graph_respects_depth_limit() {
        let store = setup_cross_agent_graph().await;
        let walker = GraphWalker::new(&store);

        let mut allowed = HashSet::new();
        allowed.insert("agent-2".to_string());

        // Depth 0: only the start page
        let result = walker
            .subgraph_cross_agent("agent-1", "Research", 0, 100, 100_000, &allowed)
            .await
            .unwrap();

        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.pages[0].title, "Research");
    }

    #[tokio::test]
    async fn test_cross_agent_write_denied() {
        // Verify that ScratchpadStore does NOT allow writing to another agent's pages
        // by checking that write_page is always scoped to the provided agent_id.
        // This is an architectural invariant — there is no tool that writes cross-agent.
        let store = ScratchpadStore::in_memory().unwrap();

        store
            .write_page("agent-1", "Private", "Secret data", &[])
            .await
            .unwrap();

        // Agent-2 writing a page with the same title creates their OWN page, not overwriting agent-1's
        store
            .write_page("agent-2", "Private", "Agent 2 data", &[])
            .await
            .unwrap();

        let page1 = store.read_page("agent-1", "Private").await.unwrap();
        let page2 = store.read_page("agent-2", "Private").await.unwrap();

        assert_eq!(page1.content, "Secret data");
        assert_eq!(page2.content, "Agent 2 data");
        assert_ne!(page1.id, page2.id);
    }
}
