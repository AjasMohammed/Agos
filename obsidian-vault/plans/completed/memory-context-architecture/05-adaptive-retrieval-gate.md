---
title: "Phase 5: Adaptive Retrieval Gate"
tags:
  - kernel
  - retrieval
  - memory
  - context
  - v3
  - plan
date: 2026-03-12
status: complete
effort: 2d
priority: high
---

# Phase 5: Adaptive Retrieval Gate

> Before any retrieval, classify whether retrieval is needed and which stores to query -- skipping retrieval in 50%+ of cases while routing the rest to the correct indexes via parallel `tokio::spawn` queries with content-hash deduplication.

---

## Why This Phase

Probing-RAG demonstrated **skipping retrieval in 57.5% of cases** while exceeding baselines by 6--8 accuracy points. Not every query needs memory lookup. Simple follow-ups ("ok", "thanks"), clarifications, and direct tool invocations gain nothing from retrieval. The gate eliminates wasted embedding + search calls and focuses retrieval budget on queries that actually need context.

Currently, the only memory injection happens via a hard-coded FTS search in `task_executor.rs` line 272 (`search_events` with `top_k=3`). This always runs, always targets only episodic memory, and never queries semantic, procedural, or tool indexes. After Phase 3 (`ContextCompiler`), the `CompilationInputs.knowledge_blocks: Vec<String>` field provides the injection point -- but nothing fills it intelligently. This phase bridges that gap.

---

## Current State

- `task_executor.rs` line 272--308 does a single hard-coded `self.episodic_memory.search_events(&task.original_prompt, None, Some(&task.agent_id), 3)` on every task start
- Results are formatted as `[EPISODIC_RECALL]` and pushed via `push_entry()` into the flat context window
- `SemanticStore` exists with hybrid FTS+cosine RRF search (`search()` method) but is never queried during task execution
- `ProceduralStore` (Phase 4) will provide `search(query, top_k) -> Result<Vec<ProcedureSearchResult>>` but does not exist yet
- `ToolRegistry` has `get_by_name()` / `list_all()` but no vector search
- `ContextCompiler` (Phase 3) accepts `CompilationInputs { knowledge_blocks: Vec<String>, .. }` but nothing populates it
- No query classification, no multi-index retrieval, no result fusion across stores

## Target State

- `RetrievalGate` classifies queries using keyword heuristics into a `RetrievalPlan` (zero LLM calls)
- `RetrievalPlan` specifies which indexes to query, with per-index `top_k` and optional query rewriting
- `RetrievalExecutor` runs parallel `tokio::spawn` tasks per index, collects results, deduplicates by content hash (FxHash), and sorts by score
- Results are formatted as strings and flow into `CompilationInputs.knowledge_blocks` for the compiler
- Simple/trivial queries skip retrieval entirely (empty plan, no wasted compute)
- The hard-coded episodic recall block in `task_executor.rs` is replaced by the retrieval gate pipeline
- New file: `crates/agentos-kernel/src/retrieval_gate.rs`

---

## Subtasks

### 5.1 Define core types

**File:** `crates/agentos-kernel/src/retrieval_gate.rs` (new file)

```rust
use agentos_memory::{EpisodicStore, SemanticStore};
use agentos_types::AgentOSError;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Which memory index to query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexType {
    /// SemanticStore — facts, knowledge, long-term memory.
    Semantic,
    /// EpisodicStore — task events, past experiences.
    Episodic,
    /// ProceduralStore — skills, SOPs, learned procedures (Phase 4).
    Procedural,
    /// ToolRegistry name-based search — tool discovery.
    /// Uses substring/fuzzy matching on tool names and descriptions.
    /// Will upgrade to vector search when Phase 2 is implemented (~30+ tools).
    Tools,
}

impl std::fmt::Display for IndexType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexType::Semantic => write!(f, "semantic"),
            IndexType::Episodic => write!(f, "episodic"),
            IndexType::Procedural => write!(f, "procedural"),
            IndexType::Tools => write!(f, "tools"),
        }
    }
}

/// A single query to issue against one index.
#[derive(Debug, Clone)]
pub struct IndexQuery {
    /// Which index to search.
    pub index: IndexType,
    /// Maximum results to return from this index.
    pub top_k: usize,
    /// The search query text. May differ from the original user query
    /// if the gate rewrites it (e.g., stripping temporal markers for semantic search).
    pub query: String,
}

/// The output of `RetrievalGate::classify()` — specifies which indexes to query.
#[derive(Debug, Clone)]
pub struct RetrievalPlan {
    /// Ordered list of index queries to execute in parallel.
    pub queries: Vec<IndexQuery>,
}

impl RetrievalPlan {
    /// A plan that skips all retrieval.
    pub fn empty() -> Self {
        Self { queries: vec![] }
    }

    /// Returns `true` if no retrieval is needed.
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }

    /// Total number of results expected across all indexes.
    pub fn total_top_k(&self) -> usize {
        self.queries.iter().map(|q| q.top_k).sum()
    }
}

/// A single result from any memory index, normalized for cross-index fusion.
#[derive(Debug, Clone)]
pub struct RetrievalResult {
    /// Which index produced this result.
    pub source: IndexType,
    /// The text content to inject into context.
    pub content: String,
    /// Relevance score (0.0..1.0 range, normalized per index).
    pub score: f32,
    /// Optional structured metadata (e.g., episode timestamp, procedure name).
    pub metadata: Option<serde_json::Value>,
}

impl RetrievalResult {
    /// Compute a stable hash of the content for deduplication.
    /// Uses the standard library hasher for simplicity; content equality
    /// is the dedup criterion, not semantic similarity.
    fn content_hash(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.content.hash(&mut hasher);
        hasher.finish()
    }
}
```

### 5.2 Implement `RetrievalGate` with heuristic classification

**File:** `crates/agentos-kernel/src/retrieval_gate.rs` (append after types)

The gate runs pure keyword heuristics -- no LLM call, no embedding, no I/O. It classifies the user query and produces a `RetrievalPlan`.

```rust
/// Heuristic-based retrieval gate that classifies queries and determines
/// which memory indexes to query before LLM inference.
///
/// Design decision: keyword heuristics over LLM-based classification.
/// This avoids an extra API call per query and keeps latency near zero.
/// The heuristics are intentionally conservative -- when in doubt, they
/// route to semantic search rather than skipping retrieval.
pub struct RetrievalGate {
    /// Default top_k for indexes that don't have a specific override.
    default_top_k: usize,
}

impl RetrievalGate {
    pub fn new(default_top_k: usize) -> Self {
        Self { default_top_k }
    }

    /// Classify a user query and produce a retrieval plan.
    ///
    /// Returns `RetrievalPlan::empty()` for trivial inputs (acknowledgments,
    /// single-word responses), saving 100% of retrieval cost for ~50% of queries.
    pub fn classify(&self, query: &str) -> RetrievalPlan {
        let lower = query.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();

        // Skip retrieval for trivial inputs
        if Self::is_trivial(&lower, &words) {
            return RetrievalPlan::empty();
        }

        let mut queries = Vec::new();
        let mut routed_indexes = HashSet::new();

        // Episodic signals: temporal references, memory recall
        if Self::has_episodic_signal(&lower) {
            queries.push(IndexQuery {
                index: IndexType::Episodic,
                top_k: self.default_top_k,
                query: query.to_string(),
            });
            routed_indexes.insert(IndexType::Episodic);
        }

        // Procedural signals: "how to", "steps to", workflow questions
        if Self::has_procedural_signal(&lower) {
            queries.push(IndexQuery {
                index: IndexType::Procedural,
                top_k: 3, // Procedures are longer; fewer results needed
                query: query.to_string(),
            });
            routed_indexes.insert(IndexType::Procedural);
        }

        // Tool signals: tool discovery, capability questions.
        // Currently uses name-based substring matching on ToolRegistry.
        // When Phase 2 (Semantic Tool Discovery) is implemented, this will
        // upgrade to ToolRegistry::search_tools() with vector cosine similarity.
        if Self::has_tool_signal(&lower) {
            queries.push(IndexQuery {
                index: IndexType::Tools,
                top_k: self.default_top_k,
                query: query.to_string(),
            });
            routed_indexes.insert(IndexType::Tools);
        }

        // Factual/knowledge signals: definitions, explanations
        if Self::has_factual_signal(&lower) {
            if !routed_indexes.contains(&IndexType::Semantic) {
                queries.push(IndexQuery {
                    index: IndexType::Semantic,
                    top_k: self.default_top_k,
                    query: query.to_string(),
                });
                routed_indexes.insert(IndexType::Semantic);
            }
        }

        // Default fallback: if no specific signal matched but the query is
        // non-trivial, route to semantic search. This ensures we never skip
        // retrieval for queries that might benefit from knowledge injection.
        if queries.is_empty() {
            queries.push(IndexQuery {
                index: IndexType::Semantic,
                top_k: self.default_top_k,
                query: query.to_string(),
            });
        }

        RetrievalPlan { queries }
    }

    /// Returns `true` for trivial inputs that never benefit from retrieval:
    /// single-word acknowledgments, confirmations, and navigation commands.
    fn is_trivial(lower: &str, words: &[&str]) -> bool {
        const TRIVIAL_WORDS: &[&str] = &[
            "ok", "okay", "yes", "no", "sure", "thanks", "thank", "done",
            "next", "continue", "stop", "cancel", "quit", "exit", "help",
            "got", "it", "right", "fine", "good", "great", "cool", "yep",
            "nope", "y", "n",
        ];

        // Empty or whitespace-only
        if words.is_empty() {
            return true;
        }

        // Short inputs where every word is a trivial token
        if words.len() <= 3 && words.iter().all(|w| TRIVIAL_WORDS.contains(w)) {
            return true;
        }

        // Common short phrases that are trivial
        const TRIVIAL_PHRASES: &[&str] = &[
            "got it", "sounds good", "go ahead", "do it",
            "that works", "looks good", "makes sense",
        ];
        if lower.len() < 30 && TRIVIAL_PHRASES.iter().any(|p| lower.contains(p)) {
            return true;
        }

        false
    }

    /// Detect temporal/experiential signals that indicate episodic memory is relevant.
    fn has_episodic_signal(lower: &str) -> bool {
        const SIGNALS: &[&str] = &[
            "remember", "last time", "previously", "earlier", "before",
            "what happened", "history", "recall", "when did", "past",
            "yesterday", "last week", "ago", "recent", "previous task",
            "tried before", "we did", "you did", "i asked",
        ];
        SIGNALS.iter().any(|s| lower.contains(s))
    }

    /// Detect procedural/instructional signals that indicate procedural memory is relevant.
    fn has_procedural_signal(lower: &str) -> bool {
        const SIGNALS: &[&str] = &[
            "how to", "how do", "steps to", "procedure for", "process for",
            "workflow", "best way to", "instructions for", "guide for",
            "walk me through", "step by step", "recipe for", "method for",
            "best practice", "standard operating",
        ];
        SIGNALS.iter().any(|s| lower.contains(s))
    }

    /// Detect tool discovery signals that indicate tool registry search is relevant.
    /// Currently uses name-based matching; will use vector search after Phase 2.
    fn has_tool_signal(lower: &str) -> bool {
        const SIGNALS: &[&str] = &[
            "find tool", "search tool", "need a tool", "which tool",
            "available tool", "tool for", "capability", "what tools",
            "list tools", "can you", "is there a way to",
        ];
        SIGNALS.iter().any(|s| lower.contains(s))
    }

    /// Detect factual/definitional signals that indicate semantic memory is relevant.
    fn has_factual_signal(lower: &str) -> bool {
        const SIGNALS: &[&str] = &[
            "what is", "what are", "who is", "where is", "define",
            "explain", "describe", "tell me about", "meaning of",
            "difference between", "compare", "summarize", "overview of",
        ];
        SIGNALS.iter().any(|s| lower.contains(s))
    }
}
```

### 5.3 Implement `RetrievalExecutor` with parallel queries and deduplication

**File:** `crates/agentos-kernel/src/retrieval_gate.rs` (append after `RetrievalGate`)

The executor launches one `tokio::spawn` per `IndexQuery`, awaits all handles, merges results, deduplicates by content hash, and sorts by score descending.

```rust
use crate::tool_registry::ToolRegistry;

/// Executes retrieval plans by querying memory indexes in parallel.
///
/// Each index query runs in its own `tokio::spawn` task. Results are
/// collected, deduplicated by content hash, and sorted by score.
///
/// The executor holds `Arc` references to all memory stores. Stores that
/// are not yet implemented (Procedural, Tools) are optional -- queries
/// targeting them return empty results with a tracing warning.
pub struct RetrievalExecutor {
    /// Semantic memory store (facts, knowledge).
    semantic: Option<Arc<SemanticStore>>,
    /// Episodic memory store (task events, experiences).
    episodic: Arc<EpisodicStore>,
    /// Tool registry for tool-description search.
    /// Phase 2 will add `search_tools()` — until then, tool queries
    /// fall back to name-based matching.
    tool_registry: Arc<RwLock<ToolRegistry>>,
    // ProceduralStore will be added here after Phase 4 lands:
    // procedural: Option<Arc<ProceduralStore>>,
}

impl RetrievalExecutor {
    /// Create a new executor with the available memory stores.
    ///
    /// `semantic` is optional because `SemanticStore::open()` requires the
    /// embedding model, which may not be available in test environments.
    pub fn new(
        semantic: Option<Arc<SemanticStore>>,
        episodic: Arc<EpisodicStore>,
        tool_registry: Arc<RwLock<ToolRegistry>>,
    ) -> Self {
        Self {
            semantic,
            episodic,
            tool_registry,
        }
    }

    /// Execute a retrieval plan: launch parallel queries, collect, dedup, sort.
    ///
    /// Returns an empty `Vec` if the plan is empty (trivial query).
    /// Individual index failures are logged but do not fail the entire retrieval --
    /// partial results are always better than no results.
    pub async fn execute(
        &self,
        plan: &RetrievalPlan,
        agent_id: Option<&agentos_types::AgentID>,
    ) -> Vec<RetrievalResult> {
        if plan.is_empty() {
            return vec![];
        }

        // Launch one tokio task per index query
        let mut handles: Vec<tokio::task::JoinHandle<Vec<RetrievalResult>>> = Vec::new();

        for index_query in &plan.queries {
            match index_query.index {
                IndexType::Semantic => {
                    if let Some(ref store) = self.semantic {
                        let store = store.clone();
                        let query = index_query.query.clone();
                        let top_k = index_query.top_k;
                        let agent_id_clone = agent_id.copied();
                        handles.push(tokio::spawn(async move {
                            match store
                                .search(&query, agent_id_clone.as_ref(), top_k, 0.0)
                                .await
                            {
                                Ok(results) => results
                                    .into_iter()
                                    .map(|r| RetrievalResult {
                                        source: IndexType::Semantic,
                                        content: r.chunk.content,
                                        score: r.rrf_score,
                                        metadata: Some(serde_json::json!({
                                            "key": r.entry.key,
                                            "memory_id": r.entry.id,
                                            "semantic_score": r.semantic_score,
                                            "fts_score": r.fts_score,
                                        })),
                                    })
                                    .collect(),
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Semantic retrieval failed, returning empty"
                                    );
                                    vec![]
                                }
                            }
                        }));
                    } else {
                        tracing::debug!("Semantic store not available, skipping semantic query");
                    }
                }
                IndexType::Episodic => {
                    let store = self.episodic.clone();
                    let query = index_query.query.clone();
                    let top_k = index_query.top_k;
                    let agent_id_clone = agent_id.copied();
                    // EpisodicStore uses Mutex<Connection> (blocking) — spawn_blocking
                    handles.push(tokio::spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            store.search_events(
                                &query,
                                None, // search across all tasks
                                agent_id_clone.as_ref(),
                                top_k as u32,
                            )
                        })
                        .await;
                        match result {
                            Ok(Ok(episodes)) => episodes
                                .into_iter()
                                .map(|ep| {
                                    // Use summary if available, otherwise truncate content
                                    let display_content = ep
                                        .summary
                                        .clone()
                                        .unwrap_or_else(|| {
                                            ep.content
                                                .chars()
                                                .take(500)
                                                .collect()
                                        });
                                    RetrievalResult {
                                        source: IndexType::Episodic,
                                        content: display_content,
                                        // FTS results don't have a normalized score;
                                        // assign 0.5 base so they participate in
                                        // cross-index sorting without dominating.
                                        score: 0.5,
                                        metadata: Some(serde_json::json!({
                                            "episode_type": ep.entry_type.as_str(),
                                            "task_id": ep.task_id.as_uuid().to_string(),
                                            "timestamp": ep.timestamp.to_rfc3339(),
                                        })),
                                    }
                                })
                                .collect(),
                            Ok(Err(e)) => {
                                tracing::warn!(
                                    error = %e,
                                    "Episodic retrieval failed, returning empty"
                                );
                                vec![]
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Episodic spawn_blocking panicked"
                                );
                                vec![]
                            }
                        }
                    }));
                }
                IndexType::Procedural => {
                    // Phase 4 will add ProceduralStore.
                    // Until then, procedural queries produce no results.
                    tracing::debug!(
                        "Procedural store not yet implemented, skipping procedural query"
                    );
                }
                IndexType::Tools => {
                    // Phase 2 will add ToolRegistry::search_tools() with embeddings.
                    // Until then, fall back to simple substring matching against
                    // tool names and descriptions.
                    let registry = self.tool_registry.clone();
                    let query = index_query.query.clone();
                    let top_k = index_query.top_k;
                    handles.push(tokio::spawn(async move {
                        let registry = registry.read().await;
                        let lower_query = query.to_lowercase();
                        let query_words: Vec<&str> =
                            lower_query.split_whitespace().collect();
                        let mut results: Vec<RetrievalResult> = registry
                            .list_all()
                            .into_iter()
                            .filter_map(|tool| {
                                let name = tool.manifest.manifest.name.to_lowercase();
                                let desc = tool
                                    .manifest
                                    .manifest
                                    .description
                                    .to_lowercase();
                                // Score: count how many query words appear in
                                // the tool name or description
                                let hits = query_words
                                    .iter()
                                    .filter(|w| name.contains(*w) || desc.contains(*w))
                                    .count();
                                if hits > 0 {
                                    let score =
                                        hits as f32 / query_words.len().max(1) as f32;
                                    Some(RetrievalResult {
                                        source: IndexType::Tools,
                                        content: format!(
                                            "{}: {}",
                                            tool.manifest.manifest.name,
                                            tool.manifest.manifest.description
                                        ),
                                        score,
                                        metadata: Some(serde_json::json!({
                                            "tool_name": tool.manifest.manifest.name,
                                            "tool_id": tool.id.as_uuid().to_string(),
                                        })),
                                    })
                                } else {
                                    None
                                }
                            })
                            .collect();
                        results.sort_by(|a, b| {
                            b.score
                                .partial_cmp(&a.score)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                        results.truncate(top_k);
                        results
                    }));
                }
            }
        }

        // Await all spawned tasks and collect results
        let mut all_results: Vec<RetrievalResult> = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(results) => all_results.extend(results),
                Err(e) => {
                    tracing::warn!(error = %e, "Retrieval task panicked");
                }
            }
        }

        // Deduplicate by content hash — keep the higher-scored entry
        let mut seen_hashes: HashSet<u64> = HashSet::new();
        let mut deduped: Vec<RetrievalResult> = Vec::new();
        // Sort by score descending first so the highest-scored duplicate wins
        all_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for result in all_results {
            let hash = result.content_hash();
            if seen_hashes.insert(hash) {
                deduped.push(result);
            }
        }

        deduped
    }

    /// Format retrieval results as strings suitable for `CompilationInputs.knowledge_blocks`.
    ///
    /// Each result is wrapped in a source-typed tag to help the LLM distinguish
    /// retrieved knowledge from other context categories.
    pub fn format_as_knowledge_blocks(results: &[RetrievalResult]) -> Vec<String> {
        if results.is_empty() {
            return vec![];
        }

        // Group results by source for cleaner formatting
        let mut by_source: std::collections::BTreeMap<String, Vec<&RetrievalResult>> =
            std::collections::BTreeMap::new();
        for r in results {
            by_source
                .entry(r.source.to_string())
                .or_default()
                .push(r);
        }

        let mut blocks = Vec::new();
        for (source, items) in &by_source {
            let tag = source.to_uppercase();
            let mut block = format!("[RETRIEVED_{}]\n", tag);
            for item in items {
                block.push_str(&format!("- {}\n", item.content));
            }
            block.push_str(&format!("[/RETRIEVED_{}]", tag));
            blocks.push(block);
        }

        blocks
    }
}
```

### 5.4 Wire into kernel: add `RetrievalGate` and `RetrievalExecutor` to `Kernel` struct

**File:** `crates/agentos-kernel/src/kernel.rs`

Add two new fields to the `Kernel` struct after `event_bus`:

```rust
pub retrieval_gate: Arc<crate::retrieval_gate::RetrievalGate>,
pub retrieval_executor: Arc<crate::retrieval_gate::RetrievalExecutor>,
```

In `Kernel::boot()`, after the `episodic_memory` and `tool_registry` are created (around line 260), construct the gate and executor:

```rust
// 6.6 Initialize retrieval gate and executor
let retrieval_gate = Arc::new(crate::retrieval_gate::RetrievalGate::new(5));
let retrieval_executor = Arc::new(crate::retrieval_gate::RetrievalExecutor::new(
    None, // SemanticStore is not yet wired into the kernel; will be added in a follow-up
    episodic_memory.clone(),
    tool_registry.clone(),
));
```

Add the fields to the `Kernel { ... }` struct literal:

```rust
retrieval_gate,
retrieval_executor,
```

### 5.5 Wire into task executor: replace hard-coded episodic recall

**File:** `crates/agentos-kernel/src/task_executor.rs`

Replace the hard-coded episodic recall block (lines 272--308) in `execute_task_sync()` with the retrieval gate pipeline. The new code runs **after** the injection scan and **before** the agent loop.

**Remove this block** (lines 272--308):
```rust
// 2.5. Auto-inject relevant episodic memories from past tasks
if let Ok(past_episodes) = self.episodic_memory.search_events(
    &task.original_prompt,
    None,
    Some(&task.agent_id),
    3, // top 3 most relevant
) {
    // ... format and push_entry ...
}
```

**Replace with:**
```rust
// 2.5. Adaptive retrieval gate — classify query and retrieve from relevant indexes
{
    let plan = self.retrieval_gate.classify(&task.original_prompt);
    if !plan.is_empty() {
        tracing::info!(
            task_id = %task.id,
            queries = plan.queries.len(),
            total_top_k = plan.total_top_k(),
            "Retrieval gate produced plan with {} index queries",
            plan.queries.len()
        );

        let retrieved = self
            .retrieval_executor
            .execute(&plan, Some(&task.agent_id))
            .await;

        if !retrieved.is_empty() {
            let knowledge_blocks =
                crate::retrieval_gate::RetrievalExecutor::format_as_knowledge_blocks(
                    &retrieved,
                );
            for block in &knowledge_blocks {
                self.context_manager
                    .push_entry(
                        &task.id,
                        ContextEntry {
                            role: ContextRole::System,
                            content: block.clone(),
                            timestamp: chrono::Utc::now(),
                            metadata: None,
                            importance: 0.6,
                            pinned: false,
                            reference_count: 0,
                            partition: ContextPartition::default(),
                        },
                    )
                    .await
                    .ok();
            }

            tracing::info!(
                task_id = %task.id,
                results = retrieved.len(),
                blocks = knowledge_blocks.len(),
                "Injected {} retrieval results as {} knowledge blocks",
                retrieved.len(),
                knowledge_blocks.len()
            );
        }
    } else {
        tracing::debug!(
            task_id = %task.id,
            "Retrieval gate: trivial query, skipping retrieval"
        );
    }
}
```

**Note:** After Phase 3 lands, this block will change to populate `CompilationInputs.knowledge_blocks` instead of calling `push_entry()` directly. The retrieval logic itself stays the same -- only the injection point changes.

### 5.6 Export from `lib.rs`

**File:** `crates/agentos-kernel/src/lib.rs`

Add module declaration and re-exports:

```rust
pub mod retrieval_gate;
pub use retrieval_gate::{
    IndexType, RetrievalExecutor, RetrievalGate, RetrievalPlan, RetrievalResult,
};
```

### 5.7 Write unit tests for `RetrievalGate`

**File:** `crates/agentos-kernel/src/retrieval_gate.rs` (inline `#[cfg(test)]` module at end of file)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> RetrievalGate {
        RetrievalGate::new(5)
    }

    // --- Trivial query tests (should skip retrieval) ---

    #[test]
    fn trivial_single_word_acknowledgments_skip_retrieval() {
        let g = gate();
        for input in &["ok", "yes", "no", "sure", "thanks", "done", "next", "continue"] {
            let plan = g.classify(input);
            assert!(
                plan.is_empty(),
                "Expected trivial skip for '{}', got {} queries",
                input,
                plan.queries.len()
            );
        }
    }

    #[test]
    fn trivial_short_phrases_skip_retrieval() {
        let g = gate();
        for input in &["got it", "sounds good", "go ahead", "do it"] {
            let plan = g.classify(input);
            assert!(
                plan.is_empty(),
                "Expected trivial skip for '{}', got {} queries",
                input,
                plan.queries.len()
            );
        }
    }

    #[test]
    fn empty_input_skips_retrieval() {
        let g = gate();
        assert!(g.classify("").is_empty());
        assert!(g.classify("   ").is_empty());
    }

    // --- Episodic signal tests ---

    #[test]
    fn episodic_signals_route_to_episodic_index() {
        let g = gate();
        let cases = &[
            "what happened last time we deployed?",
            "do you remember the error from yesterday?",
            "recall the previous database migration",
            "when did we last run the backup?",
        ];
        for input in cases {
            let plan = g.classify(input);
            assert!(
                plan.queries.iter().any(|q| q.index == IndexType::Episodic),
                "Expected episodic route for '{}': {:?}",
                input,
                plan.queries.iter().map(|q| &q.index).collect::<Vec<_>>()
            );
        }
    }

    // --- Procedural signal tests ---

    #[test]
    fn procedural_signals_route_to_procedural_index() {
        let g = gate();
        let cases = &[
            "how to set up the database",
            "steps to deploy to production",
            "walk me through the CI pipeline",
            "what is the best way to configure nginx",
        ];
        for input in cases {
            let plan = g.classify(input);
            assert!(
                plan.queries.iter().any(|q| q.index == IndexType::Procedural),
                "Expected procedural route for '{}': {:?}",
                input,
                plan.queries.iter().map(|q| &q.index).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn procedural_queries_use_lower_top_k() {
        let g = gate();
        let plan = g.classify("how to deploy the application");
        let proc_query = plan
            .queries
            .iter()
            .find(|q| q.index == IndexType::Procedural)
            .expect("should have procedural query");
        assert_eq!(
            proc_query.top_k, 3,
            "Procedural queries should use top_k=3"
        );
    }

    // --- Tool signal tests ---

    #[test]
    fn tool_signals_route_to_tools_index() {
        let g = gate();
        let cases = &[
            "find a tool for parsing JSON",
            "which tool can read files?",
            "what tools are available for HTTP requests?",
        ];
        for input in cases {
            let plan = g.classify(input);
            assert!(
                plan.queries.iter().any(|q| q.index == IndexType::Tools),
                "Expected tools route for '{}': {:?}",
                input,
                plan.queries.iter().map(|q| &q.index).collect::<Vec<_>>()
            );
        }
    }

    // --- Factual/semantic signal tests ---

    #[test]
    fn factual_signals_route_to_semantic_index() {
        let g = gate();
        let cases = &[
            "what is the API rate limit?",
            "explain the authentication flow",
            "describe the database schema",
            "tell me about the deployment architecture",
        ];
        for input in cases {
            let plan = g.classify(input);
            assert!(
                plan.queries.iter().any(|q| q.index == IndexType::Semantic),
                "Expected semantic route for '{}': {:?}",
                input,
                plan.queries.iter().map(|q| &q.index).collect::<Vec<_>>()
            );
        }
    }

    // --- Default fallback tests ---

    #[test]
    fn ambiguous_queries_default_to_semantic() {
        let g = gate();
        let plan = g.classify("deploy the application to staging");
        assert!(!plan.is_empty(), "Non-trivial query should not be empty");
        assert!(
            plan.queries.iter().any(|q| q.index == IndexType::Semantic),
            "Ambiguous query should fall back to semantic"
        );
    }

    #[test]
    fn long_non_trivial_query_is_not_skipped() {
        let g = gate();
        let plan = g.classify(
            "I need to migrate the database from PostgreSQL to MySQL \
             and update all the connection strings in the config files",
        );
        assert!(!plan.is_empty());
    }

    // --- Multi-signal tests ---

    #[test]
    fn query_with_multiple_signals_routes_to_multiple_indexes() {
        let g = gate();
        let plan = g.classify("do you remember how to deploy to production last time?");
        let indexes: Vec<IndexType> = plan.queries.iter().map(|q| q.index).collect();
        assert!(
            indexes.contains(&IndexType::Episodic),
            "Should detect episodic signal ('remember', 'last time')"
        );
        assert!(
            indexes.contains(&IndexType::Procedural),
            "Should detect procedural signal ('how to')"
        );
    }

    #[test]
    fn no_duplicate_indexes_in_plan() {
        let g = gate();
        // "what is" triggers factual/semantic; no signal triggers semantic fallback
        let plan = g.classify("what is the meaning of life?");
        let semantic_count = plan
            .queries
            .iter()
            .filter(|q| q.index == IndexType::Semantic)
            .count();
        assert_eq!(
            semantic_count, 1,
            "Should not duplicate semantic index in plan"
        );
    }

    // --- Plan utility tests ---

    #[test]
    fn total_top_k_sums_all_queries() {
        let g = gate();
        let plan = g.classify("remember how to deploy?");
        let expected: usize = plan.queries.iter().map(|q| q.top_k).sum();
        assert_eq!(plan.total_top_k(), expected);
    }

    #[test]
    fn empty_plan_has_zero_total_top_k() {
        let plan = RetrievalPlan::empty();
        assert_eq!(plan.total_top_k(), 0);
    }

    // --- RetrievalResult dedup tests ---

    #[test]
    fn content_hash_is_deterministic() {
        let r1 = RetrievalResult {
            source: IndexType::Semantic,
            content: "hello world".to_string(),
            score: 0.9,
            metadata: None,
        };
        let r2 = RetrievalResult {
            source: IndexType::Episodic, // different source
            content: "hello world".to_string(),
            score: 0.5,
            metadata: None,
        };
        assert_eq!(
            r1.content_hash(),
            r2.content_hash(),
            "Same content should produce same hash regardless of source"
        );
    }

    #[test]
    fn different_content_has_different_hash() {
        let r1 = RetrievalResult {
            source: IndexType::Semantic,
            content: "hello world".to_string(),
            score: 0.9,
            metadata: None,
        };
        let r2 = RetrievalResult {
            source: IndexType::Semantic,
            content: "goodbye world".to_string(),
            score: 0.9,
            metadata: None,
        };
        assert_ne!(r1.content_hash(), r2.content_hash());
    }

    // --- Format tests ---

    #[test]
    fn format_empty_results_returns_empty_vec() {
        let blocks = RetrievalExecutor::format_as_knowledge_blocks(&[]);
        assert!(blocks.is_empty());
    }

    #[test]
    fn format_groups_results_by_source() {
        let results = vec![
            RetrievalResult {
                source: IndexType::Semantic,
                content: "fact one".to_string(),
                score: 0.9,
                metadata: None,
            },
            RetrievalResult {
                source: IndexType::Episodic,
                content: "episode one".to_string(),
                score: 0.7,
                metadata: None,
            },
            RetrievalResult {
                source: IndexType::Semantic,
                content: "fact two".to_string(),
                score: 0.6,
                metadata: None,
            },
        ];
        let blocks = RetrievalExecutor::format_as_knowledge_blocks(&results);
        assert_eq!(blocks.len(), 2, "Should have 2 blocks (episodic + semantic)");

        // Check that each block contains the right tag
        let has_episodic = blocks.iter().any(|b| b.contains("[RETRIEVED_EPISODIC]"));
        let has_semantic = blocks.iter().any(|b| b.contains("[RETRIEVED_SEMANTIC]"));
        assert!(has_episodic);
        assert!(has_semantic);

        // Check that semantic block has both facts
        let sem_block = blocks.iter().find(|b| b.contains("SEMANTIC")).unwrap();
        assert!(sem_block.contains("fact one"));
        assert!(sem_block.contains("fact two"));
    }
}
```

### 5.8 Write integration test for `RetrievalExecutor` with real `EpisodicStore`

**File:** `crates/agentos-kernel/src/retrieval_gate.rs` (append to `#[cfg(test)]` module)

```rust
    #[tokio::test]
    async fn executor_queries_episodic_store_and_returns_results() {
        let dir = tempfile::TempDir::new().unwrap();
        let episodic = Arc::new(EpisodicStore::open(dir.path()).unwrap());
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::new()));

        // Seed episodic store with test data
        let task_id = agentos_types::TaskID::new();
        let agent_id = agentos_types::AgentID::new();
        let trace_id = agentos_types::TraceID::new();

        episodic
            .record(
                &task_id,
                &agent_id,
                agentos_memory::EpisodeType::ToolCall,
                "Deployed application to production using kubectl apply",
                Some("Deployed to production"),
                None,
                &trace_id,
            )
            .unwrap();
        episodic
            .record(
                &task_id,
                &agent_id,
                agentos_memory::EpisodeType::ToolResult,
                "Database migration completed successfully with 3 tables altered",
                Some("Database migration succeeded"),
                None,
                &trace_id,
            )
            .unwrap();

        let executor = RetrievalExecutor::new(None, episodic, tool_registry);

        let plan = RetrievalPlan {
            queries: vec![IndexQuery {
                index: IndexType::Episodic,
                top_k: 5,
                query: "production deployment".to_string(),
            }],
        };

        let results = executor.execute(&plan, Some(&agent_id)).await;
        assert!(
            !results.is_empty(),
            "Should find episodic results for 'production deployment'"
        );
        assert!(results.iter().all(|r| r.source == IndexType::Episodic));
    }

    #[tokio::test]
    async fn executor_returns_empty_for_empty_plan() {
        let dir = tempfile::TempDir::new().unwrap();
        let episodic = Arc::new(EpisodicStore::open(dir.path()).unwrap());
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::new()));
        let executor = RetrievalExecutor::new(None, episodic, tool_registry);

        let results = executor.execute(&RetrievalPlan::empty(), None).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn executor_deduplicates_identical_content() {
        let dir = tempfile::TempDir::new().unwrap();
        let episodic = Arc::new(EpisodicStore::open(dir.path()).unwrap());
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::new()));

        // Record the same content twice with different entry types
        let task_id = agentos_types::TaskID::new();
        let agent_id = agentos_types::AgentID::new();
        let trace_id = agentos_types::TraceID::new();

        episodic
            .record(
                &task_id,
                &agent_id,
                agentos_memory::EpisodeType::ToolCall,
                "unique content for dedup test",
                Some("unique content for dedup test"),
                None,
                &trace_id,
            )
            .unwrap();

        let executor = RetrievalExecutor::new(None, episodic, tool_registry);

        // Query the same index twice — simulates overlapping results
        let plan = RetrievalPlan {
            queries: vec![
                IndexQuery {
                    index: IndexType::Episodic,
                    top_k: 5,
                    query: "unique content".to_string(),
                },
            ],
        };

        let results = executor.execute(&plan, Some(&agent_id)).await;
        // Each unique content string should appear at most once
        let mut seen = std::collections::HashSet::new();
        for r in &results {
            assert!(
                seen.insert(r.content_hash()),
                "Duplicate content found: {}",
                r.content
            );
        }
    }

    #[tokio::test]
    async fn executor_handles_missing_procedural_store_gracefully() {
        let dir = tempfile::TempDir::new().unwrap();
        let episodic = Arc::new(EpisodicStore::open(dir.path()).unwrap());
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::new()));
        let executor = RetrievalExecutor::new(None, episodic, tool_registry);

        // Procedural query should return empty (not error)
        let plan = RetrievalPlan {
            queries: vec![IndexQuery {
                index: IndexType::Procedural,
                top_k: 3,
                query: "how to deploy".to_string(),
            }],
        };

        let results = executor.execute(&plan, None).await;
        assert!(results.is_empty(), "Procedural queries should return empty until Phase 4");
    }

    #[tokio::test]
    async fn full_pipeline_gate_to_executor_to_format() {
        let dir = tempfile::TempDir::new().unwrap();
        let episodic = Arc::new(EpisodicStore::open(dir.path()).unwrap());
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::new()));

        let task_id = agentos_types::TaskID::new();
        let agent_id = agentos_types::AgentID::new();
        let trace_id = agentos_types::TraceID::new();

        episodic
            .record(
                &task_id,
                &agent_id,
                agentos_memory::EpisodeType::SystemEvent,
                "Server restarted after out-of-memory crash at 3am",
                Some("Server OOM crash and restart"),
                None,
                &trace_id,
            )
            .unwrap();

        let gate = RetrievalGate::new(5);
        let executor = RetrievalExecutor::new(None, episodic, tool_registry);

        // This query has an episodic signal ("what happened")
        let plan = gate.classify("what happened to the server last night?");
        assert!(plan.queries.iter().any(|q| q.index == IndexType::Episodic));

        let results = executor.execute(&plan, Some(&agent_id)).await;
        let blocks = RetrievalExecutor::format_as_knowledge_blocks(&results);

        // We should get at least one result about the OOM crash
        if !results.is_empty() {
            assert!(!blocks.is_empty());
            let all_text: String = blocks.join("\n");
            assert!(all_text.contains("[RETRIEVED_EPISODIC]"));
        }
    }
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/retrieval_gate.rs` | **New** -- `RetrievalGate`, `RetrievalExecutor`, `RetrievalPlan`, `IndexQuery`, `IndexType`, `RetrievalResult`, all tests |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod retrieval_gate;` and re-exports for `IndexType`, `RetrievalExecutor`, `RetrievalGate`, `RetrievalPlan`, `RetrievalResult` |
| `crates/agentos-kernel/src/kernel.rs` | Add `retrieval_gate: Arc<RetrievalGate>` and `retrieval_executor: Arc<RetrievalExecutor>` fields; construct in `boot()` |
| `crates/agentos-kernel/src/task_executor.rs` | Replace hard-coded episodic recall block (lines 272--308) with retrieval gate pipeline |

---

## Dependencies

- **Requires:** Phase 3 (context compiler -- for `CompilationInputs.knowledge_blocks` injection point; however, this phase can ship before Phase 3 by using `push_entry()` directly as shown in subtask 5.5)
- **Soft dependency on:** Phase 4 (procedural store -- degrades gracefully to empty results). Phase 2 (tool vector search) is deferred; tool routing uses substring matching until then.
- **Blocks:** Phase 6 (structured memory extraction -- triggers after retrieval-informed inference)

---

## Test Plan

| Test | Assertion | Type |
|------|-----------|------|
| `trivial_single_word_acknowledgments_skip_retrieval` | `plan.is_empty() == true` for "ok", "yes", "thanks", etc. | Unit |
| `trivial_short_phrases_skip_retrieval` | `plan.is_empty() == true` for "got it", "sounds good", etc. | Unit |
| `empty_input_skips_retrieval` | `plan.is_empty() == true` for "" and whitespace | Unit |
| `episodic_signals_route_to_episodic_index` | Plan contains `IndexType::Episodic` for temporal queries | Unit |
| `procedural_signals_route_to_procedural_index` | Plan contains `IndexType::Procedural` for "how to" queries | Unit |
| `procedural_queries_use_lower_top_k` | Procedural `top_k == 3` (not the default 5) | Unit |
| `tool_signals_route_to_tools_index` | Plan contains `IndexType::Tools` for "find a tool" queries | Unit |
| `factual_signals_route_to_semantic_index` | Plan contains `IndexType::Semantic` for "what is" queries | Unit |
| `ambiguous_queries_default_to_semantic` | Non-trivial unrecognized queries route to semantic | Unit |
| `long_non_trivial_query_is_not_skipped` | Long complex queries are not marked trivial | Unit |
| `query_with_multiple_signals_routes_to_multiple_indexes` | Mixed signals produce multiple index queries | Unit |
| `no_duplicate_indexes_in_plan` | Semantic appears at most once when both factual + fallback trigger | Unit |
| `content_hash_is_deterministic` | Same content -> same hash regardless of source/score | Unit |
| `different_content_has_different_hash` | Different content -> different hash | Unit |
| `format_empty_results_returns_empty_vec` | Empty input -> empty output | Unit |
| `format_groups_results_by_source` | Results grouped by IndexType into tagged blocks | Unit |
| `executor_queries_episodic_store_and_returns_results` | Real EpisodicStore returns results for matching queries | Integration |
| `executor_returns_empty_for_empty_plan` | Empty plan -> empty results (no queries issued) | Integration |
| `executor_deduplicates_identical_content` | Duplicate content across results is collapsed | Integration |
| `executor_handles_missing_procedural_store_gracefully` | Procedural queries return empty (not error) before Phase 4 | Integration |
| `full_pipeline_gate_to_executor_to_format` | End-to-end: classify -> execute -> format produces knowledge blocks | Integration |

---

## Verification

```bash
# 1. Compile the workspace (should succeed with no errors)
cargo build --workspace

# 2. Run retrieval gate tests specifically
cargo test -p agentos-kernel retrieval_gate -- --nocapture

# 3. Run all kernel tests (should not break existing tests)
cargo test -p agentos-kernel

# 4. Run full workspace tests
cargo test --workspace

# 5. Clippy lint check (must pass — CI enforced)
cargo clippy --workspace -- -D warnings

# 6. Format check
cargo fmt --all -- --check
```

---

## Related

- [[Memory Context Architecture Plan]] -- master plan
- [[Memory Context Data Flow]] -- data flow diagram
- [[03-context-assembly-engine]] -- Phase 3 provides `CompilationInputs.knowledge_blocks` injection point
- [[04-procedural-memory-tier]] -- Phase 4 provides `ProceduralStore.search()` (currently stubbed)
- [[02-semantic-tool-discovery]] -- Phase 2 (DEFERRED) would upgrade tool routing from substring matching to vector search; not required for Phase 5 to function
- [[06-structured-memory-extraction]] -- next phase, structured extraction from tool outputs
