---
title: "Phase 2: Capability Tags & Tool Grouping"
tags:
  - tools
  - agents
  - v4
  - plan
date: 2026-04-07
status: planned
effort: 0.5d
priority: medium
---

# Phase 2: Capability Tags & Tool Grouping

> Add optional `capability_tags` to tool manifests and embed them alongside descriptions for richer semantic search.

---

## Why This Phase

Tool descriptions are written for documentation, not for semantic matching. A tool named `scratch-write` with description "Write a page to the agent scratchpad" won't rank high for the query "temporary working memory." Capability tags bridge this gap: `capability_tags = ["working-memory", "temporary-storage", "scratchpad", "note-taking"]` gives the embedder richer signal.

---

## Current → Target State

**Current:** Tool manifests have `name`, `description`, and other fields. No tags for capability categorization. The `suggest` section (Phase 1) embeds `name: description` as the search corpus.

**Target:** Tool manifests support an optional `capability_tags` array. Tags are embedded alongside the description for richer search signal. The `tools` section of agent-manual groups tools by tag category.

---

## Detailed Subtasks

### 1. Add `capability_tags` to ToolManifest

**File:** `crates/agentos-types/src/tool.rs`

```rust
/// Optional semantic tags for capability discovery.
/// Free-text strings embedded alongside the description for search.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub capability_tags: Vec<String>,
```

### 2. Include tags in embedding text

**File:** `crates/agentos-tools/src/agent_manual.rs`

Update the embedding construction in `new_with_embedder`:

```rust
let text = format!(
    "{}: {} [{}]",
    ts.name,
    ts.description,
    ts.capability_tags.join(", ")
);
```

Update `ToolSummary` to carry tags:

```rust
pub struct ToolSummary {
    pub name: String,
    pub description: String,
    pub version: String,
    pub permissions: Vec<String>,
    pub capability_tags: Vec<String>, // NEW
}
```

### 3. Add tags to core tool manifests

Tag the most commonly needed tools. Examples:

| Tool | Tags |
|------|------|
| `scratch-write` | `working-memory, temporary-storage, scratchpad, notes` |
| `scratch-read` | `working-memory, scratchpad, notes, recall` |
| `memory-write` | `long-term-memory, knowledge-base, facts, storage` |
| `memory-search` | `knowledge-retrieval, search, recall, facts` |
| `spawn-agent` | `delegation, parallelism, multi-agent, sub-task` |
| `await-agents` | `synchronization, multi-agent, results, coordination` |
| `file-write` | `file-io, persistence, disk, output` |
| `http-client` | `network, api, web, request, fetch` |
| `shell-exec` | `system, command, process, automation` |
| `think` | `reasoning, planning, analysis, internal-monologue` |

### 4. Add grouped view to `tools` manual section

**File:** `crates/agentos-tools/src/agent_manual.rs`

When the `tools` section is queried, optionally group tools by their first tag:

```json
{
  "section": "tools",
  "groups": {
    "working-memory": ["scratch-write", "scratch-read", "memory-block-write", ...],
    "file-io": ["file-read", "file-write", "file-editor", ...],
    "multi-agent": ["spawn-agent", "await-agents", "verify-output", ...],
    ...
  },
  "ungrouped": ["think", "datetime", ...]
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/tool.rs` | Add `capability_tags` field to `ToolManifest` |
| `crates/agentos-tools/src/agent_manual.rs` | Update `ToolSummary`, embedding text, grouped tools view |
| `tools/core/*.toml` | Add `capability_tags` to ~15 high-value tool manifests |

---

## Dependencies

- **Requires:** Phase 1 (semantic search infrastructure)
- **Blocks:** Phase 3 (proactive hints use tags for matching)

---

## Test Plan

1. **Tags improve search** — query "temporary working memory" with and without tags; verify `scratch-write` ranks higher with tags
2. **Empty tags backward compat** — manifests without `capability_tags` parse correctly with empty vec
3. **Grouped view** — query `tools` section; verify grouped output contains expected categories

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-tools -- agent_manual
cargo test -p agentos-types -- tool_manifest
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
