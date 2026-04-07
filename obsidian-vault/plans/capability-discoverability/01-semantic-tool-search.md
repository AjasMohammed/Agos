---
title: "Phase 1: Semantic Tool Search in Agent Manual"
tags:
  - kernel
  - tools
  - agents
  - v4
  - plan
date: 2026-04-07
status: complete
effort: 1d
priority: high
---

# Phase 1: Semantic Tool Search in Agent Manual

> Add a `suggest` section to the `agent-manual` tool that accepts free-text queries and returns semantically ranked tool recommendations.

---

## Why This Phase

The `agent-manual` tool currently requires the agent to know which section to query (e.g., `tools`, `memory`, `coordination`). If an agent doesn't know that a scratchpad exists, it won't ask about the `memory` section. A semantic search over tool descriptions bridges this gap: the agent describes what it *wants to do*, and the system finds tools that match.

---

## Current → Target State

**Current:** `agent-manual` has 14 hardcoded sections. The `tools` section lists all tools; `tool-detail` gives details for a specific tool by name. No fuzzy or intent-based search.

**Target:** A new `suggest` section accepts `{ "section": "suggest", "query": "save intermediate results for later" }` and returns the top 5 tools ranked by semantic similarity to the query, with descriptions and usage hints.

---

## Detailed Subtasks

### 1. Build tool embedding index at construction time

**File:** `crates/agentos-tools/src/agent_manual.rs`

The `AgentManualTool` already receives `Vec<ToolSummary>` at construction. Extend it to build an embedding index:

```rust
pub struct AgentManualTool {
    tool_summaries: Vec<ToolSummary>,
    /// Pre-computed embeddings for tool descriptions (name + description + tags).
    tool_embeddings: Vec<(String, Vec<f32>)>, // (tool_name, embedding)
    embedder: Option<Arc<agentos_memory::Embedder>>,
}

impl AgentManualTool {
    pub fn new_with_embedder(
        tool_summaries: Vec<ToolSummary>,
        embedder: Arc<agentos_memory::Embedder>,
    ) -> Self {
        let tool_embeddings = tool_summaries.iter().map(|ts| {
            let text = format!("{}: {}", ts.name, ts.description);
            let embedding = embedder.embed(&text).unwrap_or_default();
            (ts.name.clone(), embedding)
        }).collect();

        Self {
            tool_summaries,
            tool_embeddings,
            embedder: Some(embedder),
        }
    }
}
```

### 2. Add `Suggest` variant to `ManualSection`

**File:** `crates/agentos-tools/src/agent_manual.rs`

```rust
pub enum ManualSection {
    // ... existing variants
    Suggest,
}

impl ManualSection {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            // ... existing matches
            "suggest" => Some(Self::Suggest),
            _ => None,
        }
    }

    pub fn all_names() -> &'static [&'static str] {
        &[
            // ... existing names
            "suggest",
        ]
    }
}
```

### 3. Implement suggest handler in execute()

**File:** `crates/agentos-tools/src/agent_manual.rs`

In the `execute()` match arm for `ManualSection::Suggest`:

```rust
ManualSection::Suggest => {
    let query = payload.get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AgentOSError::SchemaValidation(
            "suggest section requires 'query' field".into()
        ))?;

    let embedder = self.embedder.as_ref()
        .ok_or_else(|| AgentOSError::ToolExecutionFailed(
            "Embedding model not available for semantic search".into()
        ))?;

    let query_embedding = embedder.embed(query)
        .map_err(|e| AgentOSError::ToolExecutionFailed(format!("Embed failed: {e}")))?;

    // Cosine similarity ranking
    let mut scores: Vec<(usize, f32)> = self.tool_embeddings.iter()
        .enumerate()
        .map(|(i, (_, emb))| (i, cosine_similarity(&query_embedding, emb)))
        .collect();
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top_k = 5;
    let suggestions: Vec<Value> = scores.iter()
        .take(top_k)
        .filter(|(_, score)| *score > 0.3) // minimum relevance threshold
        .map(|(idx, score)| {
            let ts = &self.tool_summaries[*idx];
            serde_json::json!({
                "tool": ts.name,
                "description": ts.description,
                "relevance": format!("{:.2}", score),
                "permissions": ts.permissions,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "section": "suggest",
        "query": query,
        "suggestions": suggestions,
    }))
}
```

### 4. Add cosine similarity utility

**File:** `crates/agentos-tools/src/agent_manual.rs`

```rust
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { return 0.0; }
    dot / (norm_a * norm_b)
}
```

### 5. Update ToolRunner to pass embedder

**File:** `crates/agentos-tools/src/runner.rs`

Change `register_agent_manual` to accept an optional embedder:

```rust
pub fn register_agent_manual(
    &mut self,
    tool_summaries: Vec<ToolSummary>,
    embedder: Option<Arc<agentos_memory::Embedder>>,
) {
    let tool = match embedder {
        Some(emb) => AgentManualTool::new_with_embedder(tool_summaries, emb),
        None => AgentManualTool::new(tool_summaries), // fallback: no suggest
    };
    self.register(Box::new(tool));
}
```

### 6. Update tool manifest

**File:** `tools/core/agent-manual.toml`

Add `suggest` to the input schema's `section` enum.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/agent_manual.rs` | Add `Suggest` variant, embedding index, suggest handler, cosine similarity |
| `crates/agentos-tools/src/runner.rs` | Pass embedder to `register_agent_manual` |
| `crates/agentos-kernel/src/kernel.rs` | Pass shared embedder when calling `register_agent_manual` |
| `tools/core/agent-manual.toml` | Add `suggest` to section enum in schema |

---

## Dependencies

- **Requires:** None (existing embedder infra in `agentos-memory`)
- **Blocks:** Phase 2 (capability tags), Phase 3 (proactive hints)

---

## Test Plan

1. **Suggest returns ranked results** — query "save intermediate results"; verify `scratch-write` or `memory-block-write` appears in top 5
2. **Suggest with no matches** — query "quantum teleportation"; verify empty results (all below 0.3 threshold)
3. **Suggest without embedder** — construct `AgentManualTool` without embedder; verify suggest returns a clear error, not a panic
4. **Backward compat** — existing sections (`tools`, `index`, etc.) still work unchanged

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-tools -- agent_manual
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
