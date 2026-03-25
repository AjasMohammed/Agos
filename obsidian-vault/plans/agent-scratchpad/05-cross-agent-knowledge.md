---
title: "Phase 5: Cross-Agent Knowledge Sharing"
tags:
  - memory
  - scratchpad
  - security
  - v3
  - plan
date: 2026-03-23
status: planned
effort: 1.5d
priority: medium
---

# Phase 5: Cross-Agent Knowledge Sharing

> Enable agents to read (not write) each other's scratchpad pages with capability-gated access, creating emergent cross-agent knowledge graphs.

---

## Why This Phase

In multi-agent scenarios, agents working on related tasks generate complementary knowledge. Agent A investigating a bug might write notes about error patterns. Agent B working on the fix would benefit from reading those notes without Agent A needing to explicitly share them.

Cross-agent scratchpad access transforms isolated agent notebooks into a collaborative knowledge network — while maintaining security boundaries through capability tokens.

---

## Current → Target State

**Current:** All scratchpad operations are scoped to `agent_id`. No cross-agent visibility.

**Target:**
- `@agent_id/Page Title` syntax in tools to reference other agents' pages
- New permission: `scratchpad:read:<agent_id>` — grants read access to a specific agent's scratchpad
- Wildcard permission: `scratchpad:read:*` — read any agent's scratchpad
- Cross-agent backlinks tracked in `link_index` (with `is_cross_agent = true`)
- Cross-agent graph traversal in `GraphWalker` (respects capability tokens)
- Migration tool from `MemoryBlockStore` to scratchpad (optional, for existing agents)

---

## Detailed Subtasks

### 1. Parse `@agent_id/title` syntax in tools

In `scratch_read.rs` and `scratch_search.rs`, detect the `@` prefix:

```rust
fn parse_page_ref(title: &str) -> PageRef {
    if let Some(stripped) = title.strip_prefix('@') {
        if let Some((agent_id, page_title)) = stripped.split_once('/') {
            return PageRef::CrossAgent {
                agent_id: agent_id.to_string(),
                title: page_title.to_string(),
            };
        }
    }
    PageRef::SameAgent { title: title.to_string() }
}

enum PageRef {
    SameAgent { title: String },
    CrossAgent { agent_id: String, title: String },
}
```

### 2. Add capability check for cross-agent reads

In the kernel dispatch for `scratch_read`:

```rust
"scratch_read" => {
    let ref_ = parse_page_ref(title);
    match ref_ {
        PageRef::SameAgent { title } => {
            // Normal read — requires scratchpad:read
            kernel.scratchpad_store.read_page(agent_id, &title).await?
        }
        PageRef::CrossAgent { agent_id: target_agent, title } => {
            // Cross-agent — requires scratchpad:read:<target_agent>
            let required = format!("scratchpad:read:{}", target_agent);
            capability_token.check_permission(&required)?;
            kernel.scratchpad_store.read_page(&target_agent, &title).await?
        }
    }
}
```

### 3. Cross-agent graph traversal

Update `GraphWalker::subgraph()` to optionally follow cross-agent links:

```rust
pub async fn subgraph_cross_agent(
    &self,
    agent_id: &str,
    start_title: &str,
    max_depth: usize,
    max_pages: usize,
    max_bytes: usize,
    allowed_agents: &HashSet<String>,  // Agents we have permission to read
) -> Result<SubgraphResult, ScratchError>;
```

When encountering a cross-agent link during BFS:
- Check if the target agent is in `allowed_agents`
- If yes, traverse into their scratchpad
- If no, skip that edge (don't fail — just don't follow)

### 4. Add `scratchpad:read:<id>` permission pattern

Extend `PermissionSet::check()` to recognize parameterized scratchpad permissions:
- `scratchpad:read` — own scratchpad only
- `scratchpad:read:<agent_id>` — specific agent
- `scratchpad:read:*` — any agent

This follows the existing pattern in `PermissionSet` for path-prefix matching.

### 5. Optional: MemoryBlock migration tool

A kernel command or CLI command that migrates existing `MemoryBlockStore` entries to scratchpad pages:

```rust
pub async fn migrate_memory_blocks(
    block_store: &MemoryBlockStore,
    scratch_store: &ScratchpadStore,
    agent_id: &str,
) -> Result<usize, AgentOSError> {
    let blocks = block_store.list(agent_id).await?;
    for block in &blocks {
        scratch_store.write_page(
            agent_id,
            &block.label,  // label becomes title
            &block.content,
            &[],            // no tags
        ).await?;
    }
    Ok(blocks.len())
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/scratch_read.rs` | Add `@agent_id/title` parsing |
| `crates/agentos-tools/src/scratch_search.rs` | Add cross-agent search support |
| `crates/agentos-kernel/src/task_executor.rs` | Add capability check for cross-agent dispatch |
| `crates/agentos-scratch/src/graph.rs` | Add `subgraph_cross_agent()` method |
| `crates/agentos-capability/src/permission.rs` | Support `scratchpad:read:<id>` pattern |
| `crates/agentos-kernel/src/migration.rs` | **New** (optional) — MemoryBlock → Scratchpad migration |

---

## Dependencies

- **Requires:** Phase 3 (tools), Phase 4 (graph traversal)
- **Blocks:** Nothing — this is a leaf phase

---

## Test Plan

| Test | Assertion |
|------|-----------|
| `test_cross_agent_read_with_permission` | Agent A can read Agent B's page with `scratchpad:read:<B>` |
| `test_cross_agent_read_denied` | Agent A without permission gets `PermissionDenied` |
| `test_wildcard_permission` | `scratchpad:read:*` grants access to any agent |
| `test_cross_agent_graph` | BFS follows cross-agent links when permitted |
| `test_cross_agent_graph_stops_at_boundary` | BFS skips cross-agent links when not permitted |
| `test_cross_agent_write_denied` | No tool allows writing to another agent's scratchpad |
| `test_migration_from_blocks` | MemoryBlock entries become scratchpad pages with matching title/content |

---

## Verification

```bash
cargo test -p agentos-scratch -- cross
cargo test -p agentos-kernel -- scratch
cargo test -p agentos-capability -- scratchpad
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Related

- [[03-scratchpad-tools]]
- [[04-graph-context-injection]]
- [[Agent Scratchpad Plan]]
