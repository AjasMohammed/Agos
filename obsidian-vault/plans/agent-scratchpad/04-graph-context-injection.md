---
title: "Phase 4: Graph Traversal & Context Injection"
tags:
  - memory
  - scratchpad
  - context
  - v3
  - plan
date: 2026-03-23
status: planned
effort: 2d
priority: high
---

# Phase 4: Graph Traversal & Context Injection

> Implement BFS graph traversal over the scratchpad link graph and automatic injection of related notes into the LLM context window.

---

## Why This Phase

This is the scratchpad's killer feature. Without it, agents must explicitly search for and read related notes — the same workflow as semantic memory. With graph-aware context injection, the system **automatically surfaces related knowledge** by walking the link graph from the current topic.

This mirrors how a human uses Obsidian's "local graph view" — you open a note, glance at what's linked nearby, and that peripheral context helps you think. For agents, we inject that peripheral context directly into the LLM prompt.

---

## Current → Target State

**Current:** `ScratchpadStore` has `get_backlinks()` and `get_outlinks()` for single-hop queries. No multi-hop traversal. No automatic context injection.

**Target:**
- `GraphWalker` struct in `agentos-scratch` — BFS traversal with configurable depth, max_pages, max_bytes
- `ContextEntry::ScratchpadNote` variant in `agentos-types` — represents an injected scratch note
- `ContextManager` integration — before LLM inference, optionally inject related scratchpad notes
- Kernel config `[scratchpad]` section with `context_depth`, `max_context_pages`, `max_context_bytes`

---

## Detailed Subtasks

### 1. Implement `GraphWalker` in `agentos-scratch`

New file: `crates/agentos-scratch/src/graph.rs`

```rust
use crate::store::ScratchpadStore;
use crate::types::ScratchPage;

pub struct GraphWalker<'a> {
    store: &'a ScratchpadStore,
}

#[derive(Debug, Clone)]
pub struct SubgraphResult {
    pub pages: Vec<ScratchPage>,
    pub edges: Vec<(String, String)>,  // (source_title, target_title)
    pub total_bytes: usize,
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
        max_depth: usize,      // default: 2
        max_pages: usize,       // default: 5
        max_bytes: usize,       // default: 8192
    ) -> Result<SubgraphResult, ScratchError> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new(); // (title, depth)
        let mut result_pages: Vec<ScratchPage> = Vec::new();
        let mut result_edges: Vec<(String, String)> = Vec::new();
        let mut total_bytes: usize = 0;

        queue.push_back((start_title.to_string(), 0));
        visited.insert(start_title.to_string());

        while let Some((title, depth)) = queue.pop_front() {
            // Check limits
            if result_pages.len() >= max_pages || total_bytes >= max_bytes {
                break;
            }

            // Fetch page
            match self.store.read_page(agent_id, &title).await {
                Ok(page) => {
                    let page_size = page.content.len();
                    if total_bytes + page_size > max_bytes && !result_pages.is_empty() {
                        break; // Don't exceed byte budget (always include at least the start page)
                    }
                    total_bytes += page_size;
                    result_pages.push(page);
                }
                Err(ScratchError::PageNotFound { .. }) => continue, // Unresolved link
                Err(e) => return Err(e),
            }

            // Don't expand beyond max depth
            if depth >= max_depth {
                continue;
            }

            // Get neighbors (both directions)
            let outlinks = self.store.get_outlinks(agent_id, &title).await?;
            let backlinks = self.store.get_backlinks(agent_id, &title).await?;

            for target in &outlinks {
                result_edges.push((title.clone(), target.clone()));
                if !visited.contains(target) {
                    visited.insert(target.clone());
                    queue.push_back((target.clone(), depth + 1));
                }
            }
            for bl in &backlinks {
                result_edges.push((bl.title.clone(), title.clone()));
                if !visited.contains(&bl.title) {
                    visited.insert(bl.title.clone());
                    queue.push_back((bl.title.clone(), depth + 1));
                }
            }
        }

        Ok(SubgraphResult { pages: result_pages, edges: result_edges, total_bytes })
    }
}
```

### 2. Add `ContextEntry::ScratchpadNote` variant

In `crates/agentos-types/src/context.rs` (or wherever `ContextEntry` is defined):

```rust
pub enum ContextEntry {
    // ... existing variants ...
    ScratchpadNote {
        title: String,
        content: String,
        distance: usize,  // hops from the seed page (0 = the page itself)
    },
}
```

### 3. Add kernel config section

In `config/default.toml`:

```toml
[scratchpad]
enabled = true
db_path = "scratchpad.db"
context_depth = 2          # BFS depth for context injection
max_context_pages = 5      # Max pages injected per inference
max_context_bytes = 8192   # Max total bytes injected
max_page_size = 65536      # 64KB per page
max_pages_per_agent = 1000
```

Parse this in kernel config loading.

### 4. Integrate with `ContextManager`

In `crates/agentos-kernel/src/context.rs`, add a method or hook that runs before LLM inference:

```rust
impl ContextManager {
    /// Inject relevant scratchpad notes into the context window.
    /// Called before each LLM inference call.
    pub async fn inject_scratchpad_context(
        &self,
        task_id: &TaskID,
        agent_id: &str,
        scratchpad: &ScratchpadStore,
        config: &ScratchpadConfig,
    ) -> Result<(), AgentOSError> {
        // 1. Extract topic keywords from recent context entries
        //    (last tool result, task description, recent agent messages)
        let keywords = self.extract_topic_keywords(task_id)?;

        // 2. Search scratchpad for matching pages
        let matches = scratchpad.search(agent_id, &keywords, &[], 3).await
            .unwrap_or_default();

        if matches.is_empty() {
            return Ok(()); // No relevant scratch notes
        }

        // 3. Use the top match as seed for graph traversal
        let walker = GraphWalker::new(scratchpad);
        let subgraph = walker.subgraph(
            agent_id,
            &matches[0].page.title,
            config.context_depth,
            config.max_context_pages,
            config.max_context_bytes,
        ).await?;

        // 4. Inject pages as context entries
        for (i, page) in subgraph.pages.iter().enumerate() {
            self.add_entry(task_id, ContextEntry::ScratchpadNote {
                title: page.title.clone(),
                content: page.content.clone(),
                distance: i, // approximate — BFS order
            })?;
        }

        Ok(())
    }
}
```

### 5. Update `scratch-graph` tool to use `GraphWalker`

The `scratch-graph` tool from Phase 3 initially returned flat link info. Now update it to use `GraphWalker::subgraph()` for proper multi-hop traversal.

### 6. Add topic extraction heuristic

Simple keyword extraction from recent context:
- Take the last 3 context entries (tool results, agent messages)
- Extract significant words (skip stopwords, keep nouns/verbs)
- Join as FTS5 query string
- This doesn't need to be perfect — it's a heuristic seed for graph traversal

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-scratch/src/graph.rs` | **New** — `GraphWalker` with BFS traversal |
| `crates/agentos-scratch/src/lib.rs` | Export `graph` module |
| `crates/agentos-types/src/context.rs` | Add `ContextEntry::ScratchpadNote` variant |
| `crates/agentos-kernel/src/context.rs` | Add `inject_scratchpad_context()` method |
| `crates/agentos-kernel/src/task_executor.rs` | Call `inject_scratchpad_context()` before LLM inference |
| `crates/agentos-tools/src/scratch_graph.rs` | Update to use `GraphWalker::subgraph()` |
| `config/default.toml` | Add `[scratchpad]` config section |

---

## Dependencies

- **Requires:** Phase 2 (link index for BFS), Phase 3 (tools for graph tool update)
- **Blocks:** Phase 5 (cross-agent reads traverse the graph)

---

## Test Plan

| Test | Assertion |
|------|-----------|
| `test_bfs_depth_0` | Returns only the start page |
| `test_bfs_depth_1` | Returns start page + directly linked pages |
| `test_bfs_depth_2` | Returns start + neighbors + neighbors-of-neighbors |
| `test_bfs_max_pages` | Stops collecting after max_pages reached |
| `test_bfs_max_bytes` | Stops collecting after max_bytes exceeded |
| `test_bfs_cycle` | Pages linking to each other don't cause infinite loop |
| `test_bfs_unresolved_links` | Links to non-existent pages are skipped gracefully |
| `test_bfs_includes_backlinks` | Pages linking TO the start page are included |
| `test_context_injection` | After inject, context window contains `ScratchpadNote` entries |
| `test_no_injection_when_empty` | No scratchpad pages → no context entries added |
| `test_injection_respects_budget` | Injected notes don't exceed max_context_bytes |

---

## Verification

```bash
cargo test -p agentos-scratch -- graph
cargo test -p agentos-kernel -- scratch
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Related

- [[02-wikilink-parser-and-backlinks]]
- [[03-scratchpad-tools]]
- [[Agent Scratchpad Data Flow]]
