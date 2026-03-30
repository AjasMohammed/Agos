---
title: "Phase 3: Scratchpad Tools"
tags:
  - memory
  - scratchpad
  - tools
  - v3
  - plan
date: 2026-03-23
status: complete
effort: 2d
priority: high
---

# Phase 3: Scratchpad Tools

> Expose the scratchpad as AgentTool implementations that LLMs can call: scratch-write, scratch-read, scratch-search, scratch-links, scratch-graph, scratch-delete.

---

## Why This Phase

The storage engine (Phase 1) and link index (Phase 2) are internal infrastructure. Agents interact with the scratchpad through tools — the standard AgentOS interface for all agent capabilities. Without tools, the scratchpad exists but is inaccessible to LLMs.

This phase follows the existing kernel dispatch pattern: tools return `_kernel_action` strings, and the kernel's task executor routes these to the `ScratchpadStore`.

---

## Current → Target State

**Current:** `ScratchpadStore` exists with CRUD + search + link queries, but no tool interface.

**Target:** Six new tools in `agentos-tools`:

| Tool Name | Purpose | Kernel Action |
|-----------|---------|---------------|
| `scratch-write` | Create or update a scratchpad page | `scratch_write` |
| `scratch-read` | Read a page by title | `scratch_read` |
| `scratch-search` | Full-text search with optional tag filter | `scratch_search` |
| `scratch-links` | Get backlinks, outlinks, or both for a page | `scratch_links` |
| `scratch-graph` | Get the local subgraph around a page (BFS) | `scratch_graph` |
| `scratch-delete` | Delete a page | `scratch_delete` |

Plus kernel dispatch wiring in `task_executor.rs`.

---

## Detailed Subtasks

### 1. Create tool files in `agentos-tools/src/`

Each tool follows the existing pattern (see `memory_block_read.rs` for reference):

**`scratch_write.rs`:**
```rust
pub struct ScratchWriteTool;

#[async_trait]
impl AgentTool for ScratchWriteTool {
    fn name(&self) -> &str { "scratch-write" }
    fn description(&self) -> &str {
        "Write a markdown page to the agent's scratchpad. Use [[Page Title]] to link pages. \
         Supports optional YAML frontmatter and tags."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Page title (unique within your scratchpad)", "maxLength": 200 },
                "content": { "type": "string", "description": "Markdown content. Use [[Page Title]] to link to other pages." },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional tags for categorization" }
            },
            "required": ["title", "content"]
        })
    }
    fn required_permissions(&self) -> PermissionSet { PermissionSet::new(vec!["scratchpad:write".to_string()]) }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, AgentOSError> {
        let title = input["title"].as_str().ok_or(/* validation error */)?;
        let content = input["content"].as_str().ok_or(/* validation error */)?;
        let tags: Vec<String> = /* parse from input */;

        Ok(ToolOutput::new(json!({
            "_kernel_action": "scratch_write",
            "title": title,
            "content": content,
            "tags": tags
        })))
    }
}
```

**`scratch_read.rs`:**
- Input: `{ "title": "Page Title" }` (or `"@agent_id/Page Title"` for cross-agent)
- Action: `scratch_read`
- Permissions: `scratchpad:read`

**`scratch_search.rs`:**
- Input: `{ "query": "search terms", "tags": ["optional"], "limit": 10 }`
- Action: `scratch_search`
- Permissions: `scratchpad:read`

**`scratch_links.rs`:**
- Input: `{ "title": "Page Title", "direction": "both" }` (inbound|outbound|both)
- Action: `scratch_links`
- Permissions: `scratchpad:read`

**`scratch_graph.rs`:**
- Input: `{ "title": "Page Title", "depth": 2, "max_pages": 10 }`
- Action: `scratch_graph`
- Permissions: `scratchpad:read`

**`scratch_delete.rs`:**
- Input: `{ "title": "Page Title" }`
- Action: `scratch_delete`
- Permissions: `scratchpad:write`

### 2. Register tools in tool factory

In `crates/agentos-tools/src/lib.rs` (or wherever tools are registered), add the six new tools to the tool list.

### 3. Add tool manifests

Create `tools/core/scratch-write.toml`, `scratch-read.toml`, etc.:

```toml
[tool]
name = "scratch-write"
version = "0.1.0"
description = "Write a markdown page to the agent's scratchpad"
trust_tier = "core"

[permissions]
required = ["scratchpad:write"]
```

### 4. Wire kernel dispatch in `task_executor.rs`

In the kernel's action dispatch (where `_kernel_action` values are matched), add handlers:

```rust
"scratch_write" => {
    let title = action["title"].as_str().unwrap();
    let content = action["content"].as_str().unwrap();
    let tags: Vec<String> = /* parse */;
    let page = kernel.scratchpad_store.write_page(agent_id, title, content, &tags).await?;
    json!({ "status": "ok", "page_id": page.id, "title": page.title })
}
"scratch_read" => {
    let title = action["title"].as_str().unwrap();
    let page = kernel.scratchpad_store.read_page(agent_id, title).await?;
    json!({ "title": page.title, "content": page.content, "tags": page.tags, "updated_at": page.updated_at })
}
"scratch_search" => {
    let query = action["query"].as_str().unwrap();
    let tags: Vec<String> = /* parse */;
    let limit = action["limit"].as_u64().unwrap_or(10) as usize;
    let results = kernel.scratchpad_store.search(agent_id, query, &tags, limit).await?;
    json!({ "results": results })
}
"scratch_links" => {
    let title = action["title"].as_str().unwrap();
    let info = kernel.scratchpad_store.get_all_links(agent_id, title).await?;
    json!(info)
}
"scratch_graph" => {
    // Delegates to GraphWalker (Phase 4) — for now, return links only
    let title = action["title"].as_str().unwrap();
    let info = kernel.scratchpad_store.get_all_links(agent_id, title).await?;
    json!({ "center": title, "links": info })
}
"scratch_delete" => {
    let title = action["title"].as_str().unwrap();
    kernel.scratchpad_store.delete_page(agent_id, title).await?;
    json!({ "status": "deleted", "title": title })
}
```

### 5. Add `ScratchpadStore` to Kernel struct

In `crates/agentos-kernel/src/kernel.rs`:

```rust
pub struct Kernel {
    // ... existing fields ...
    pub scratchpad_store: Arc<ScratchpadStore>,
}
```

Initialize in kernel boot:
```rust
let scratchpad_store = Arc::new(
    ScratchpadStore::new(&data_dir.join("scratchpad.db"))?
);
```

### 6. Add `scratchpad:read` and `scratchpad:write` permissions

In `agentos-types` or `agentos-capability`, ensure these permission strings are recognized. Follow the existing pattern for `memory:read`, `memory:write`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/scratch_write.rs` | **New** — `ScratchWriteTool` |
| `crates/agentos-tools/src/scratch_read.rs` | **New** — `ScratchReadTool` |
| `crates/agentos-tools/src/scratch_search.rs` | **New** — `ScratchSearchTool` |
| `crates/agentos-tools/src/scratch_links.rs` | **New** — `ScratchLinksTool` |
| `crates/agentos-tools/src/scratch_graph.rs` | **New** — `ScratchGraphTool` |
| `crates/agentos-tools/src/scratch_delete.rs` | **New** — `ScratchDeleteTool` |
| `crates/agentos-tools/src/lib.rs` | Register six new tools |
| `crates/agentos-tools/Cargo.toml` | Add `agentos-scratch` dependency |
| `crates/agentos-kernel/src/kernel.rs` | Add `scratchpad_store: Arc<ScratchpadStore>` field |
| `crates/agentos-kernel/src/task_executor.rs` | Add dispatch arms for 6 scratch actions |
| `crates/agentos-kernel/Cargo.toml` | Add `agentos-scratch` dependency |
| `tools/core/scratch-*.toml` | **New** — 6 tool manifest files |

---

## Dependencies

- **Requires:** Phase 1 (storage), Phase 2 (link parsing for write_page)
- **Blocks:** Phase 4 (context injection uses tools), Phase 5 (cross-agent), Phase 6 (auto-write)

---

## Test Plan

| Test | Assertion |
|------|-----------|
| `test_scratch_write_tool_schema` | Schema has required fields: title, content |
| `test_scratch_write_returns_action` | Output contains `_kernel_action: "scratch_write"` |
| `test_scratch_read_returns_action` | Output contains `_kernel_action: "scratch_read"` |
| `test_scratch_write_requires_permission` | Without `scratchpad:write`, execution denied |
| `test_scratch_read_requires_permission` | Without `scratchpad:read`, execution denied |
| `test_kernel_dispatch_write` | Kernel handles `scratch_write` action → page persisted |
| `test_kernel_dispatch_read` | Kernel handles `scratch_read` action → page content returned |
| `test_kernel_dispatch_search` | Kernel handles `scratch_search` action → results returned |
| `test_kernel_dispatch_links` | Kernel handles `scratch_links` action → backlinks returned |
| `test_kernel_dispatch_delete` | Kernel handles `scratch_delete` action → page removed |
| `test_tool_manifests_load` | All 6 scratch-*.toml manifests load with `trust_tier = "core"` |

---

## Verification

```bash
# Build tools and kernel with scratch support
cargo build -p agentos-tools -p agentos-kernel

# Run tool tests
cargo test -p agentos-tools -- scratch

# Run kernel dispatch tests
cargo test -p agentos-kernel -- scratch

# Full workspace
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Related

- [[01-core-storage-engine]]
- [[02-wikilink-parser-and-backlinks]]
- [[04-graph-context-injection]]
- [[Agent Scratchpad Data Flow]]
