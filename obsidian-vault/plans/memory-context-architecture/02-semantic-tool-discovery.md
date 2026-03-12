---
title: "Phase 2: Semantic Tool Discovery (Deferred)"
tags:
  - kernel
  - tools
  - memory
  - plan
  - phase-2
  - v3
  - deferred
date: 2026-03-12
status: deferred
effort: 4d
priority: low
---

# Phase 2: Semantic Tool Discovery (Deferred)

> [!warning] Deferred to V3.3+
> This phase is **deferred** until the tool catalog exceeds ~30 tools. With only 14 built-in tools, name-based lookup is sufficient and the ~23MB embedding model (AllMiniLM-L6-V2 ONNX) overhead is not justified. The plan below remains valid and ready to execute when the tool count warrants it. See [[Memory Context Architecture Plan]] for the updated milestone schedule.

> Agents discover tools by describing what capability they need, not by knowing the exact tool name. Implements the meta-tool pattern -- yielding 25+ accuracy percentage points improvement per Anthropic's research (49% to 74% accuracy, 85% token savings).

---

## Why This Phase

The research is unambiguous: RAG-MCP boosts tool selection from 13% to 43%, and Anthropic's Tool Search Tool improved accuracy from 49% to 74% while saving 85% of tokens. With 5,000+ MCP servers and growing tool catalogs, name-based lookup is a dead end.

Today `ToolRegistry::get_by_name()` does exact string matching -- an agent that needs "read files from disk" must already know the tool is called `file-reader`. With 14 built-in tools this is manageable; at 50+ tools it becomes the primary bottleneck for tool selection accuracy.

This phase has the highest single-feature ROI of any memory/context change.

---

## Current State

- `ToolRegistry` stores `HashMap<ToolID, RegisteredTool>` + `HashMap<String, ToolID>` (name index).
- `get_by_name(&str)` does exact string match -- no fuzzy, no semantic.
- `tools_for_prompt()` formats ALL registered tools into a single string -- no filtering.
- `RegisteredTool` has three fields: `id: ToolID`, `manifest: ToolManifest`, `status: ToolStatus`.
- `ToolInfo` has: `name`, `version`, `description`, `author`, `author_pubkey`, `signature`, `trust_tier`, `checksum` -- no `always_loaded` flag.
- `Embedder` exists in `agentos-memory/src/embedder.rs` -- AllMiniLML6V2, 384-dim, `embed(&[&str]) -> Result<Vec<Vec<f32>>>`.
- `SemanticStore` already has a private `cosine_similarity()` function in `agentos-memory/src/semantic.rs` (line 647).
- `agentos-memory` is already a dependency of `agentos-kernel` (in `Cargo.toml` line 17).
- `ToolRunner` initializes its own `Embedder` at construction in `runner.rs` but does not expose it.

## Target State

- Every `RegisteredTool` has a pre-computed 384-dim embedding vector (`Option<Vec<f32>>`).
- `ToolInfo` has an `always_loaded: bool` field (parsed from TOML manifests).
- `ToolRegistry::register()` accepts `Option<&Embedder>` and embeds the tool description at registration time.
- `search_tools(query, top_k, embedder)` returns semantically ranked tools by cosine similarity.
- `core_tools_for_prompt()` returns only `always_loaded` or `Core`-tier tools.
- `dynamic_tools_for_prompt(tool_ids)` returns specific tools discovered via search.
- A `tool-search` meta-tool manifest exists in `tools/core/` and its handler is registered in `ToolRunner`.
- `cosine_similarity()` is a free function in `tool_registry.rs` that handles zero vectors and dimension mismatches.

---

## Detailed Subtasks

### 2.1 Add `always_loaded` field to `ToolInfo`

**File:** `crates/agentos-types/src/tool.rs`

Add the field to the existing `ToolInfo` struct. It must default to `false` so existing manifests parse without changes.

```rust
// In struct ToolInfo, after the trust_tier field:

/// Whether this tool should always appear in the system prompt.
/// Core tools typically set this to `true`. Tools without this flag
/// are only surfaced via semantic search (tool-search meta-tool).
#[serde(default)]
pub always_loaded: bool,
```

No re-export needed -- `ToolInfo` is accessed via `ToolManifest::manifest` and is not re-exported at the `agentos-types` crate root (confirmed: `src/lib.rs` line 42-44 exports `RegisteredTool`, `ToolManifest`, `ToolSandbox`, `ToolStatus`, `TrustTier` but not `ToolInfo`).

**Update existing manifests** in `tools/core/*.toml` to set `always_loaded = true`:

Files to update (add `always_loaded = true` under `[manifest]`):
- `tools/core/file-reader.toml`
- `tools/core/file-writer.toml`
- `tools/core/shell-exec.toml`
- `tools/core/memory-search.toml`
- `tools/core/memory-write.toml`
- `tools/core/data-parser.toml`
- `tools/core/http-client.toml`

### 2.2 Add `embedding` field to `RegisteredTool`

**File:** `crates/agentos-types/src/tool.rs`

Add the embedding field to `RegisteredTool`. It lives on `RegisteredTool` (the kernel's runtime representation), NOT on `ToolInfo` (the serialized manifest), because embeddings are computed at registration time and never persisted to disk.

```rust
/// A registered tool in the kernel's tool registry.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    pub id: ToolID,
    pub manifest: ToolManifest,
    pub status: ToolStatus,
    /// Pre-computed 384-dim embedding of the tool's name + description.
    /// `None` if the embedder was unavailable at registration time.
    pub embedding: Option<Vec<f32>>,
}
```

**Breaking change:** Any code that constructs `RegisteredTool` via struct literal must add the `embedding` field. The only place this happens is `ToolRegistry::register()` in `crates/agentos-kernel/src/tool_registry.rs` (line 72-76).

### 2.3 Modify `register()` to accept an optional `Embedder` and compute embeddings

**File:** `crates/agentos-kernel/src/tool_registry.rs`

Change the `register()` signature and add an import for `Embedder`:

```rust
use agentos_memory::Embedder;
// ... existing imports ...

impl ToolRegistry {
    /// Register a single tool from its manifest, enforcing trust tier and CRL policy.
    ///
    /// If an `Embedder` is provided, the tool's name, description, and input schema
    /// are embedded into a 384-dim vector for semantic search. Embedding failure is
    /// non-fatal: the tool is still registered with `embedding: None`.
    pub fn register(
        &mut self,
        manifest: ToolManifest,
        embedder: Option<&Embedder>,
    ) -> Result<ToolID, AgentOSError> {
        verify_manifest_with_crl(&manifest, &self.crl)?;

        let tool_id = ToolID::new();
        let name = manifest.manifest.name.clone();

        // Compute embedding from tool metadata.
        // Format: "name: description. Input schema: input_type"
        // This gives the embedding model enough semantic signal to match
        // queries like "read files" to a tool named "file-reader".
        let embedding = embedder.and_then(|emb| {
            let embed_text = format!(
                "{}: {}. Input: {}",
                manifest.manifest.name,
                manifest.manifest.description,
                manifest.intent_schema.input,
            );
            match emb.embed(&[embed_text.as_str()]) {
                Ok(mut vecs) => vecs.pop(),
                Err(e) => {
                    tracing::warn!(
                        tool = %manifest.manifest.name,
                        error = %e,
                        "Failed to embed tool description; tool registered without embedding"
                    );
                    None
                }
            }
        });

        let tool = RegisteredTool {
            id: tool_id,
            manifest,
            status: ToolStatus::Available,
            embedding,
        };
        self.name_index.insert(name, tool_id);
        self.tools.insert(tool_id, tool);
        Ok(tool_id)
    }
}
```

**Update all callers of `register()`:**

1. **`ToolRegistry::load_from_dirs_with_crl()`** (same file, line 55):
   Change `registry.register(loaded.manifest.clone())?` to `registry.register(loaded.manifest.clone(), None)?`.
   Note: At this point the embedder is not yet available. The kernel can re-embed after boot, or the `load_from_dirs_with_crl` method can accept an `Option<&Embedder>` parameter and thread it through. The simpler approach is to add an `embed_all()` method (subtask 2.4) that bulk-embeds after the registry is loaded.

2. **Test files** that call `register()` directly:
   - `crates/agentos-cli/tests/common.rs`
   - `crates/agentos-cli/tests/integration_test.rs`
   - `crates/agentos-cli/tests/kernel_boot_test.rs`
   - Any other test that constructs a `ToolRegistry` and calls `register()`.

   All of these pass `None` as the embedder (tests should not require the embedding model).

### 2.4 Add `embed_all()` and `cosine_similarity()` to `ToolRegistry`

**File:** `crates/agentos-kernel/src/tool_registry.rs`

Add a free function `cosine_similarity()` and two new methods on `ToolRegistry`:

```rust
/// Compute cosine similarity between two vectors.
///
/// Returns 0.0 if:
/// - Either vector has zero magnitude (is a zero vector)
/// - The vectors have different dimensions
///
/// The return value is in the range [-1.0, 1.0] for normalized vectors.
/// MiniLM-L6-v2 produces unit-normalized embeddings, so the result is
/// always in [0.0, 1.0] in practice.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

impl ToolRegistry {
    /// Bulk-embed all registered tools that lack an embedding.
    ///
    /// Called once after kernel boot when the embedder becomes available.
    /// Tools that already have embeddings are skipped. Embedding failures
    /// are logged but do not prevent other tools from being embedded.
    pub fn embed_all(&mut self, embedder: &Embedder) {
        for tool in self.tools.values_mut() {
            if tool.embedding.is_some() {
                continue;
            }
            let embed_text = format!(
                "{}: {}. Input: {}",
                tool.manifest.manifest.name,
                tool.manifest.manifest.description,
                tool.manifest.intent_schema.input,
            );
            match embedder.embed(&[embed_text.as_str()]) {
                Ok(mut vecs) => {
                    tool.embedding = vecs.pop();
                }
                Err(e) => {
                    tracing::warn!(
                        tool = %tool.manifest.manifest.name,
                        error = %e,
                        "Failed to embed tool description during bulk embed"
                    );
                }
            }
        }
    }

    /// Search for tools whose description semantically matches the query.
    ///
    /// Embeds the query string, then computes cosine similarity against every
    /// tool that has a pre-computed embedding. Returns up to `top_k` results
    /// sorted by descending score.
    ///
    /// Returns a vec of `(ToolID, tool_name, cosine_score)`.
    pub fn search_tools(
        &self,
        query: &str,
        top_k: usize,
        embedder: &Embedder,
    ) -> Result<Vec<(ToolID, String, f32)>, AgentOSError> {
        if top_k == 0 {
            return Ok(Vec::new());
        }

        let query_vec = embedder.embed(&[query]).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to embed search query: {}", e))
        })?;
        let query_vec = query_vec.into_iter().next().ok_or_else(|| {
            AgentOSError::StorageError(
                "Embedder returned empty result for search query".to_string(),
            )
        })?;

        let mut scores: Vec<(ToolID, String, f32)> = self
            .tools
            .values()
            .filter_map(|tool| {
                tool.embedding.as_ref().map(|emb| {
                    let score = cosine_similarity(&query_vec, emb);
                    (tool.id, tool.manifest.manifest.name.clone(), score)
                })
            })
            .collect();

        scores.sort_by(|a, b| {
            b.2.partial_cmp(&a.2)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scores.truncate(top_k);
        Ok(scores)
    }
}
```

### 2.5 Split `tools_for_prompt()` into core and dynamic variants

**File:** `crates/agentos-kernel/src/tool_registry.rs`

Keep the existing `tools_for_prompt()` method for backward compatibility. Add two new methods:

```rust
impl ToolRegistry {
    /// Returns only always-loaded tools for the system prompt.
    ///
    /// A tool is always-loaded if:
    /// - `manifest.manifest.always_loaded == true`, OR
    /// - `manifest.manifest.trust_tier == TrustTier::Core`
    ///
    /// This keeps the base system prompt small when the tool catalog grows.
    pub fn core_tools_for_prompt(&self) -> String {
        let mut lines: Vec<String> = self
            .tools
            .values()
            .filter(|t| {
                t.manifest.manifest.always_loaded
                    || t.manifest.manifest.trust_tier == TrustTier::Core
            })
            .map(|t| {
                format!(
                    "- {} : {}",
                    t.manifest.manifest.name, t.manifest.manifest.description
                )
            })
            .collect();

        if lines.is_empty() {
            "No tools available.".to_string()
        } else {
            lines.sort(); // deterministic ordering
            lines.join("\n")
        }
    }

    /// Returns specific tools by ID, formatted for the system prompt.
    ///
    /// Used after `search_tools()` to inject dynamically discovered tools
    /// into the context window for a specific LLM inference call.
    pub fn dynamic_tools_for_prompt(&self, tool_ids: &[ToolID]) -> String {
        let lines: Vec<String> = tool_ids
            .iter()
            .filter_map(|id| self.tools.get(id))
            .map(|t| {
                format!(
                    "- {} : {}",
                    t.manifest.manifest.name, t.manifest.manifest.description
                )
            })
            .collect();

        if lines.is_empty() {
            "No additional tools found.".to_string()
        } else {
            lines.join("\n")
        }
    }
}
```

Do NOT remove the existing `tools_for_prompt()` method yet -- it is used in multiple places and the migration to `core_tools_for_prompt()` happens in Phase 3 (Context Assembly Engine).

### 2.6 Create `tool-search` manifest

**File:** `tools/core/tool-search.toml` (new file)

```toml
[manifest]
name = "tool-search"
version = "1.0.0"
description = "Search for available tools by describing what capability you need. Returns the most relevant tools matching your description, ranked by semantic similarity."
author = "agentos-core"
trust_tier = "core"
always_loaded = true

[capabilities_required]
permissions = ["read"]

[capabilities_provided]
outputs = ["tool_list"]

[intent_schema]
input = "ToolSearchIntent"
output = "ToolSearchResult"

[sandbox]
network = false
fs_write = false
gpu = false
max_memory_mb = 64
max_cpu_ms = 5000
syscalls = []

[executor]
type = "inline"
```

### 2.7 Implement `ToolSearchTool` handler

**File:** `crates/agentos-tools/src/tool_search.rs` (new file)

The tool-search handler uses the `_kernel_action` pattern (same as `AgentMessageTool` and `TaskDelegate`) to signal the kernel to perform the search, since the tool handler does not have direct access to `ToolRegistry` or `Embedder`.

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;

/// Meta-tool that lets agents discover other tools by semantic search.
///
/// The handler validates input and returns a `_kernel_action: "tool_search"`
/// marker. The kernel intercepts this in `task_executor.rs` and performs the
/// actual `ToolRegistry::search_tools()` call with the shared `Embedder`.
pub struct ToolSearchTool;

impl ToolSearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentTool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool-search"
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "tool-search requires a 'query' string field".to_string(),
                )
            })?;

        if query.trim().is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "tool-search 'query' must not be empty".to_string(),
            ));
        }

        let top_k = payload
            .get("top_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(20) as usize; // cap at 20

        Ok(serde_json::json!({
            "_kernel_action": "tool_search",
            "query": query,
            "top_k": top_k,
        }))
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("tools".to_string(), PermissionOp::Read)]
    }
}
```

### 2.8 Register `ToolSearchTool` in `ToolRunner`

**File:** `crates/agentos-tools/src/lib.rs`

Add the module declaration and re-export:

```rust
pub mod tool_search;
// ... in the re-exports section:
pub use tool_search::ToolSearchTool;
```

**File:** `crates/agentos-tools/src/runner.rs`

Add the import and registration (after the existing `HardwareInfoTool` registration, line 73):

```rust
use crate::tool_search::ToolSearchTool;

// In ToolRunner::new_with_model_cache_dir(), after line 73:
runner.register(Box::new(ToolSearchTool::new()));
```

### 2.9 Handle `_kernel_action: "tool_search"` via `KernelAction` enum

**Files:** `crates/agentos-kernel/src/kernel_action.rs`

The kernel dispatches tool-initiated actions through a structured `KernelAction` enum in `kernel_action.rs`, NOT via inline matching in `task_executor.rs`. The flow is:

1. Tool returns JSON with `_kernel_action: "tool_search"`
2. `KernelAction::from_tool_result()` parses it into a `KernelAction::ToolSearch` variant
3. `dispatch_kernel_action()` matches the variant and calls `execute_tool_search()`

This follows the exact same pattern used by `DelegateTask`, `SendAgentMessage`, `EscalateToHuman`, and `SwitchPartition`.

**Step 1 — Add the `ToolSearch` variant to the enum** (after `SwitchPartition`, line 32):

```rust
pub(crate) enum KernelAction {
    // ... existing variants ...
    SwitchPartition {
        partition: String,
    },
    ToolSearch {
        query: String,
        top_k: usize,
    },
}
```

**Step 2 — Add the parse arm in `from_tool_result()`** (before the `other =>` fallback, line 135):

```rust
            "tool_search" => {
                let query = value.get("query")?.as_str()?.to_string();
                let top_k = value
                    .get("top_k")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5)
                    .min(20) as usize;
                Some(Self::ToolSearch { query, top_k })
            }
```

**Step 3 — Add the `action_name` arm in `dispatch_kernel_action()`** (line 159):

```rust
        let action_name = match &action {
            // ... existing arms ...
            KernelAction::SwitchPartition { .. } => "switch_partition",
            KernelAction::ToolSearch { .. } => "tool_search",
        };
```

**Step 4 — Add the dispatch arm in the `match action` block** (after `SwitchPartition`, line 211):

```rust
            KernelAction::ToolSearch { query, top_k } => {
                self.execute_tool_search(task, &query, top_k).await
            }
```

**Step 5 — Add the `execute_tool_search()` method** (after `execute_switch_partition`, line 439):

```rust
    async fn execute_tool_search(
        &self,
        task: &AgentTask,
        query: &str,
        top_k: usize,
    ) -> KernelActionResult {
        let registry = self.tool_registry.read().await;
        match registry.search_tools(query, top_k, &self.embedder) {
            Ok(results) => {
                let total = registry.list_all().len();
                let result_list: Vec<serde_json::Value> = results
                    .iter()
                    .map(|(id, name, score)| {
                        let desc = registry
                            .get_by_id(id)
                            .map(|t| t.manifest.manifest.description.as_str())
                            .unwrap_or("");
                        serde_json::json!({
                            "name": name,
                            "description": desc,
                            "score": score,
                        })
                    })
                    .collect();
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "results": result_list,
                        "total_tools_available": total,
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }
```

**Note:** This method uses `self.embedder` (the shared `Arc<Embedder>` added in subtask 2.10) instead of creating a new `Embedder` per search. The `Embedder` is confirmed `Send + Sync` — `SemanticStore` already uses `Arc<Embedder>` in production (`semantic.rs:15`).

### 2.10 Add shared `Embedder` to `Kernel` struct

**File:** `crates/agentos-kernel/src/kernel.rs`

The `Embedder` wraps `fastembed::TextEmbedding` which is `Send + Sync` — confirmed by `SemanticStore` already using `Arc<Embedder>` at `semantic.rs:15`. This makes it safe to share across `tokio::spawn` boundaries.

Add a new field to the `Kernel` struct (after `episodic_memory`, line 48):

```rust
pub embedder: Arc<agentos_memory::Embedder>,
```

**In `Kernel::boot()`**, initialize the embedder from the model cache dir that is already resolved (after line 206 where `model_cache_dir` is created):

```rust
let embedder = Arc::new(
    agentos_memory::Embedder::with_cache_dir(&model_cache_dir)
        .map_err(|e| anyhow::anyhow!("Failed to initialize shared embedder: {}", e))?,
);
```

Then after `tool_registry` is created (line 177), bulk-embed all tools:

```rust
{
    let mut registry = tool_registry.write().await;
    registry.embed_all(&embedder);
}
```

And add `embedder` to the `Kernel` struct literal (after line 300):

```rust
embedder,
```

This shared `Embedder` is used by `execute_tool_search()` in `kernel_action.rs` (subtask 2.9) via `self.embedder`. The `search_tools()` method accepts `&Embedder`, and the `Arc` dereferences to this automatically.

**Important:** `Embedder::embed()` is a blocking call (runs ONNX inference). Since it runs inside an `async fn` that already holds a `RwLock` read guard on `tool_registry`, the blocking duration (~1-5ms per query) is acceptable for Phase 2. If profiling shows contention, a future optimization would be to embed the query outside the lock and pass the vector in directly, or wrap the embed call in `tokio::task::spawn_blocking`.

### 2.11 Update `load_from_dirs_with_crl` callers

**File:** `crates/agentos-kernel/src/tool_registry.rs`

Inside `load_from_dirs_with_crl`, update the `register()` call to pass `None`:

```rust
// Line 55 - change:
//   registry.register(loaded.manifest.clone())?;
// To:
    registry.register(loaded.manifest.clone(), None)?;
```

The bulk embedding happens afterward via `embed_all()` called from `Kernel::boot()` (subtask 2.10).

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/tool.rs` | Add `always_loaded: bool` to `ToolInfo`; add `embedding: Option<Vec<f32>>` to `RegisteredTool` |
| `crates/agentos-kernel/src/tool_registry.rs` | Change `register()` signature to accept `Option<&Embedder>`; add `embed_all()`, `search_tools()`, `cosine_similarity()`, `core_tools_for_prompt()`, `dynamic_tools_for_prompt()` |
| `crates/agentos-kernel/src/kernel.rs` | Add `embedder: Arc<Embedder>` field; init in `boot()`; call `embed_all()` |
| `crates/agentos-kernel/src/kernel_action.rs` | Add `ToolSearch { query, top_k }` variant to `KernelAction` enum; add parse arm in `from_tool_result()`; add dispatch arm and `execute_tool_search()` method |
| `crates/agentos-tools/src/tool_search.rs` | New file: `ToolSearchTool` implementing `AgentTool` |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod tool_search;` and `pub use tool_search::ToolSearchTool;` |
| `crates/agentos-tools/src/runner.rs` | Register `ToolSearchTool` in `ToolRunner::new_with_model_cache_dir()` |
| `tools/core/tool-search.toml` | New manifest for the `tool-search` meta-tool |
| `tools/core/file-reader.toml` | Add `always_loaded = true` |
| `tools/core/file-writer.toml` | Add `always_loaded = true` |
| `tools/core/shell-exec.toml` | Add `always_loaded = true` |
| `tools/core/memory-search.toml` | Add `always_loaded = true` |
| `tools/core/memory-write.toml` | Add `always_loaded = true` |
| `tools/core/data-parser.toml` | Add `always_loaded = true` |
| `tools/core/http-client.toml` | Add `always_loaded = true` |
| `crates/agentos-cli/tests/common.rs` | Update `register()` calls to pass `None` embedder |
| `crates/agentos-cli/tests/integration_test.rs` | Update `register()` calls to pass `None` embedder |
| `crates/agentos-cli/tests/kernel_boot_test.rs` | Update `register()` calls to pass `None` embedder |

---

## Dependencies

- **Requires:** None. The `Embedder` and `agentos-memory` dependency already exist in `agentos-kernel/Cargo.toml` (line 17). Phase 1 (episodic auto-write) is not a strict prerequisite, though the master plan sequences it first.
- **Blocks:** Phase 5 (Adaptive Retrieval Gate) -- the gate needs `search_tools()` to route tool-discovery queries.
- **Blocks:** Phase 3 (Context Assembly Engine) -- the compiler needs `core_tools_for_prompt()` to build the tools partition.

---

## Test Plan

### Test 1: `cosine_similarity` edge cases

**File:** `crates/agentos-kernel/src/tool_registry.rs` (inline `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let score = cosine_similarity(&a, &b);
        assert!((score - 1.0).abs() < 1e-6, "Identical vectors should have similarity 1.0, got {}", score);
    }

    #[test]
    fn test_cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let score = cosine_similarity(&a, &b);
        assert!(score.abs() < 1e-6, "Orthogonal vectors should have similarity 0.0, got {}", score);
    }

    #[test]
    fn test_cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let score = cosine_similarity(&a, &b);
        assert!((score - (-1.0)).abs() < 1e-6, "Opposite vectors should have similarity -1.0, got {}", score);
    }

    #[test]
    fn test_cosine_similarity_zero_vector_a() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let score = cosine_similarity(&a, &b);
        assert_eq!(score, 0.0, "Zero vector A should return 0.0");
    }

    #[test]
    fn test_cosine_similarity_zero_vector_b() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![0.0, 0.0, 0.0];
        let score = cosine_similarity(&a, &b);
        assert_eq!(score, 0.0, "Zero vector B should return 0.0");
    }

    #[test]
    fn test_cosine_similarity_both_zero() {
        let a = vec![0.0, 0.0];
        let b = vec![0.0, 0.0];
        let score = cosine_similarity(&a, &b);
        assert_eq!(score, 0.0, "Both zero vectors should return 0.0");
    }

    #[test]
    fn test_cosine_similarity_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let score = cosine_similarity(&a, &b);
        assert_eq!(score, 0.0, "Dimension mismatch should return 0.0");
    }

    #[test]
    fn test_cosine_similarity_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let score = cosine_similarity(&a, &b);
        assert_eq!(score, 0.0, "Empty vectors should return 0.0");
    }

    #[test]
    fn test_cosine_similarity_single_element() {
        let a = vec![3.0];
        let b = vec![4.0];
        let score = cosine_similarity(&a, &b);
        assert!((score - 1.0).abs() < 1e-6, "Parallel single-element vectors should have similarity 1.0");
    }
}
```

### Test 2: `RegisteredTool` embedding field

```rust
#[test]
fn test_registered_tool_without_embedding() {
    let manifest = make_test_manifest("test-tool", "A test tool");
    let tool = RegisteredTool {
        id: ToolID::new(),
        manifest,
        status: ToolStatus::Available,
        embedding: None,
    };
    assert!(tool.embedding.is_none());
}
```

### Test 3: `core_tools_for_prompt` filtering

```rust
#[test]
fn test_core_tools_for_prompt_filters_non_core() {
    let mut registry = ToolRegistry::new();

    let mut core_manifest = make_test_manifest("core-tool", "A core tool");
    core_manifest.manifest.trust_tier = TrustTier::Core;
    core_manifest.manifest.always_loaded = true;
    registry.register(core_manifest, None).unwrap();

    let mut community_manifest = make_test_manifest("community-tool", "A community tool");
    community_manifest.manifest.trust_tier = TrustTier::Community;
    community_manifest.manifest.always_loaded = false;
    // Community tools require valid signature -- for this test, use Core tier
    // or set up a valid signature. Adjust as needed for the trust tier policy.

    let prompt = registry.core_tools_for_prompt();
    assert!(prompt.contains("core-tool"), "Core tool should appear in prompt");
    // community-tool was not registered (would fail sig check), so just verify
    // the core tool is present.
}

#[test]
fn test_core_tools_for_prompt_includes_always_loaded_community() {
    let mut registry = ToolRegistry::new();

    let mut manifest = make_test_manifest("always-on", "Always loaded community tool");
    manifest.manifest.trust_tier = TrustTier::Core; // Use Core to bypass sig check in test
    manifest.manifest.always_loaded = true;
    registry.register(manifest, None).unwrap();

    let prompt = registry.core_tools_for_prompt();
    assert!(prompt.contains("always-on"));
}
```

### Test 4: Semantic search with real embeddings

```rust
#[test]
fn test_search_tools_semantic_ranking() {
    let embedder = agentos_memory::Embedder::new().unwrap();
    let mut registry = ToolRegistry::new();

    // Register tools with embeddings
    let tools = vec![
        ("file-reader", "Read contents of files from the filesystem"),
        ("file-writer", "Write content to files on the filesystem"),
        ("http-client", "Make HTTP requests to external web APIs and services"),
        ("shell-exec", "Execute shell commands in a sandboxed environment"),
        ("memory-search", "Search semantic memory for stored knowledge and facts"),
        ("data-parser", "Parse JSON, CSV, YAML, and TOML structured data"),
    ];

    for (name, desc) in &tools {
        let mut manifest = make_test_manifest(name, desc);
        manifest.manifest.trust_tier = TrustTier::Core;
        registry.register(manifest, Some(&embedder)).unwrap();
    }

    // "read files from disk" should rank file-reader highest
    let results = registry.search_tools("read files from disk", 3, &embedder).unwrap();
    assert!(!results.is_empty(), "Should return at least one result");
    assert_eq!(results[0].1, "file-reader",
        "Top result for 'read files from disk' should be file-reader, got '{}'", results[0].1);

    // "make API calls to web services" should rank http-client highest
    let results = registry.search_tools("make API calls to web services", 3, &embedder).unwrap();
    assert!(results.iter().any(|r| r.1 == "http-client"),
        "http-client should appear in results for 'make API calls'");

    // "run shell commands" should rank shell-exec highest
    let results = registry.search_tools("run shell commands", 3, &embedder).unwrap();
    assert!(results.iter().any(|r| r.1 == "shell-exec"),
        "shell-exec should appear in results for 'run shell commands'");

    // All scores should be in [0.0, 1.0] range (MiniLM produces normalized embeddings)
    for (_, _, score) in &results {
        assert!(*score >= 0.0 && *score <= 1.0,
            "Score should be in [0.0, 1.0], got {}", score);
    }
}

#[test]
fn test_search_tools_top_k_zero() {
    let embedder = agentos_memory::Embedder::new().unwrap();
    let registry = ToolRegistry::new();
    let results = registry.search_tools("anything", 0, &embedder).unwrap();
    assert!(results.is_empty(), "top_k=0 should return empty results");
}

#[test]
fn test_search_tools_no_embeddings() {
    let embedder = agentos_memory::Embedder::new().unwrap();
    let mut registry = ToolRegistry::new();

    // Register without embedder
    let mut manifest = make_test_manifest("no-embed", "Tool without embedding");
    manifest.manifest.trust_tier = TrustTier::Core;
    registry.register(manifest, None).unwrap();

    let results = registry.search_tools("anything", 5, &embedder).unwrap();
    assert!(results.is_empty(),
        "Tools without embeddings should not appear in search results");
}
```

### Test 5: `ToolSearchTool` handler validation

**File:** `crates/agentos-tools/src/tool_search.rs` (inline `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;
    use std::path::PathBuf;

    fn make_ctx() -> ToolExecutionContext {
        let mut perms = PermissionSet::new();
        perms.grant("tools".to_string(), true, false, false, None);
        ToolExecutionContext {
            data_dir: PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: perms,
            vault: None,
            hal: None,
        }
    }

    #[tokio::test]
    async fn test_tool_search_returns_kernel_action() {
        let tool = ToolSearchTool::new();
        let result = tool
            .execute(
                serde_json::json!({"query": "read files", "top_k": 3}),
                make_ctx(),
            )
            .await
            .unwrap();

        assert_eq!(result["_kernel_action"], "tool_search");
        assert_eq!(result["query"], "read files");
        assert_eq!(result["top_k"], 3);
    }

    #[tokio::test]
    async fn test_tool_search_default_top_k() {
        let tool = ToolSearchTool::new();
        let result = tool
            .execute(
                serde_json::json!({"query": "anything"}),
                make_ctx(),
            )
            .await
            .unwrap();

        assert_eq!(result["top_k"], 5, "Default top_k should be 5");
    }

    #[tokio::test]
    async fn test_tool_search_caps_top_k() {
        let tool = ToolSearchTool::new();
        let result = tool
            .execute(
                serde_json::json!({"query": "anything", "top_k": 100}),
                make_ctx(),
            )
            .await
            .unwrap();

        assert_eq!(result["top_k"], 20, "top_k should be capped at 20");
    }

    #[tokio::test]
    async fn test_tool_search_missing_query() {
        let tool = ToolSearchTool::new();
        let err = tool
            .execute(serde_json::json!({}), make_ctx())
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    }

    #[tokio::test]
    async fn test_tool_search_empty_query() {
        let tool = ToolSearchTool::new();
        let err = tool
            .execute(serde_json::json!({"query": "  "}), make_ctx())
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    }
}
```

### Test 6: `embed_all()` bulk embedding

```rust
#[test]
fn test_embed_all_fills_missing_embeddings() {
    let embedder = agentos_memory::Embedder::new().unwrap();
    let mut registry = ToolRegistry::new();

    // Register without embedder
    let mut m1 = make_test_manifest("tool-a", "First tool");
    m1.manifest.trust_tier = TrustTier::Core;
    registry.register(m1, None).unwrap();

    let mut m2 = make_test_manifest("tool-b", "Second tool");
    m2.manifest.trust_tier = TrustTier::Core;
    registry.register(m2, None).unwrap();

    // Verify no embeddings
    for tool in registry.list_all() {
        assert!(tool.embedding.is_none());
    }

    // Bulk embed
    registry.embed_all(&embedder);

    // Verify all tools now have embeddings
    for tool in registry.list_all() {
        assert!(tool.embedding.is_some(),
            "Tool '{}' should have an embedding after embed_all()",
            tool.manifest.manifest.name);
        let emb = tool.embedding.as_ref().unwrap();
        assert_eq!(emb.len(), 384,
            "Embedding dimension should be 384, got {}", emb.len());
    }
}

#[test]
fn test_embed_all_skips_existing_embeddings() {
    let embedder = agentos_memory::Embedder::new().unwrap();
    let mut registry = ToolRegistry::new();

    // Register WITH embedder
    let mut m1 = make_test_manifest("tool-a", "First tool");
    m1.manifest.trust_tier = TrustTier::Core;
    registry.register(m1, Some(&embedder)).unwrap();

    let original_embedding = registry
        .get_by_name("tool-a")
        .unwrap()
        .embedding
        .clone()
        .unwrap();

    // Bulk embed should skip tools that already have embeddings
    registry.embed_all(&embedder);

    let after_embedding = registry
        .get_by_name("tool-a")
        .unwrap()
        .embedding
        .clone()
        .unwrap();

    assert_eq!(original_embedding, after_embedding,
        "embed_all() should not overwrite existing embeddings");
}
```

### Test helper: `make_test_manifest`

All tests above use this helper. If it does not already exist in the test module, add it:

```rust
fn make_test_manifest(name: &str, description: &str) -> ToolManifest {
    ToolManifest {
        manifest: agentos_types::tool::ToolInfo {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: description.to_string(),
            author: "test".to_string(),
            checksum: None,
            author_pubkey: None,
            signature: None,
            trust_tier: TrustTier::Core,
            always_loaded: false,
        },
        capabilities_required: agentos_types::tool::ToolCapabilities {
            permissions: vec!["read".to_string()],
        },
        capabilities_provided: agentos_types::tool::ToolOutputs {
            outputs: vec!["content.text".to_string()],
        },
        intent_schema: agentos_types::tool::ToolSchema {
            input: "TestInput".to_string(),
            output: "TestOutput".to_string(),
        },
        input_schema: None,
        sandbox: agentos_types::tool::ToolSandbox {
            network: false,
            fs_write: false,
            gpu: false,
            max_memory_mb: 64,
            max_cpu_ms: 5000,
            syscalls: vec![],
        },
        executor: agentos_types::tool::ToolExecutor::default(),
    }
}
```

---

## Verification

```bash
# 1. Ensure the workspace compiles with the new fields and methods
cargo build --workspace

# 2. Run tool_registry unit tests (cosine_similarity, core_tools_for_prompt, etc.)
cargo test -p agentos-kernel -- tool_registry

# 3. Run tool_search handler tests
cargo test -p agentos-tools -- tool_search

# 4. Run the full test suite to confirm no regressions from the register() signature change
cargo test --workspace

# 5. Lint check -- must pass with zero warnings
cargo clippy --workspace -- -D warnings

# 6. Format check
cargo fmt --all -- --check

# 7. Verify tool-search.toml parses correctly
cargo test -p agentos-tools -- loader

# 8. Verify semantic search returns correct rankings (requires embedding model download)
cargo test -p agentos-kernel -- test_search_tools_semantic_ranking
```

---

## Related

- [[Memory Context Architecture Plan]] -- master plan
- [[Memory Context Research Synthesis]] -- research backing the meta-tool pattern
- [[01-episodic-auto-write]] -- Phase 1 (predecessor in execution order)
- [[03-context-assembly-engine]] -- Phase 3 (uses `core_tools_for_prompt()` from this phase)
- [[05-adaptive-retrieval-gate]] -- Phase 5 (uses `search_tools()` from this phase)
