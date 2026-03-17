---
title: "Phase 6: Structured Memory Extraction"
tags:
  - plan
  - memory
  - extraction
  - kernel
  - v3
date: 2026-03-12
status: complete
effort: 2d
priority: high
---

# Phase 6: Structured Memory Extraction

> Extract salient facts from typed tool outputs using per-tool schema extractors -- not LLM calls -- eliminating the cost-doubling problem of the original Mem0-style pipeline while still enabling automatic knowledge accumulation.

---

## Why This Phase

The original Phase 6 design used an LLM call on every tool result to extract facts. This doubles API costs for every tool invocation and adds latency on the critical path. The redesigned approach exploits a key insight: **tool results are already structured**. The `http-client` tool returns `{ status, headers, body, latency_ms }`, `file-reader` returns `{ content, path }`, `shell-exec` returns `{ stdout, stderr, exit_code }`. We know the schema of every core tool's output at compile time.

Instead of asking an LLM "what facts are in this text?", we register per-tool `MemoryExtractor` implementations that deterministically pull salient fields from the result JSON. Conflict detection uses the existing `SemanticStore.search()` with a cosine threshold of 0.85 -- no LLM needed. The entire pipeline runs as an async background task that costs zero LLM tokens.

---

## Current State

- `SemanticStore` has full CRUD: `write()`, `search()`, `get_by_key()`, `delete()`
- Tool results arrive as `serde_json::Value` and are pushed via `self.context_manager.push_tool_result()` in `task_executor.rs` (line ~1192)
- No automated extraction -- all semantic writes are manual via the `memory-write` tool
- No conflict detection between new and existing memories
- `SchemaRegistry` registers input schemas at boot but has no role in output extraction
- Tools return well-typed JSON: `http-client` returns `{ status, headers, body, latency_ms, truncated }`, `file-reader` returns text content, `shell-exec` returns `{ stdout, stderr, exit_code }`, `data-parser` returns parsed structured data

## Target State

- `MemoryExtractor` trait with per-tool implementations that parse known result schemas
- `ExtractionRegistry` maps tool names to their extractors, populated at boot
- `MemoryExtractionEngine` runs as a cheap async background task after each tool result
- Conflict detection: before writing, search `SemanticStore` for cosine > 0.85 matches
- Operations: `ADD` (new fact), `UPDATE` (refine existing), `NOOP` (already captured) -- no `DELETE` (only agents delete their own memories)
- Config-driven: `[memory.extraction]` section in `config/default.toml`
- New file: `crates/agentos-kernel/src/memory_extraction.rs`
- Zero LLM cost per extraction

---

## Subtasks

### 6.1 Define types and trait

**Where:** `crates/agentos-kernel/src/memory_extraction.rs` (new file)

```rust
use agentos_memory::SemanticStore;
use agentos_types::{AgentID, AgentOSError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A single fact extracted from a tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFact {
    /// Short key for the memory entry (e.g., "http:api.example.com:200")
    pub key: String,
    /// The fact content to store in semantic memory
    pub content: String,
    /// Tags for categorization (e.g., ["http", "api", "status"])
    pub tags: Vec<String>,
}

/// Source metadata for the extraction.
#[derive(Debug, Clone)]
pub struct ExtractionContext {
    pub tool_name: String,
    pub agent_id: AgentID,
    pub task_id: agentos_types::TaskID,
}

/// What to do with an extracted fact after conflict detection.
#[derive(Debug, Clone)]
pub enum MemoryOperation {
    /// New fact -- no similar memory exists
    Add(ExtractedFact),
    /// Refine an existing memory -- cosine > threshold but content differs
    Update {
        existing_id: String,
        existing_key: String,
        new_content: String,
        tags: Vec<String>,
    },
    /// Already captured -- cosine > threshold and content is substantively the same
    Noop { reason: String },
}

/// Per-tool extractor trait. Each core tool registers an implementation that
/// knows how to pull salient facts from its typed result JSON.
pub trait MemoryExtractor: Send + Sync {
    /// The tool name this extractor handles (must match `ToolManifest.manifest.name`).
    fn tool_name(&self) -> &str;

    /// Extract zero or more facts from a tool result.
    /// Returns an empty vec if the result contains nothing worth remembering.
    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact>;
}
```

### 6.2 Implement core tool extractors

**Where:** `crates/agentos-kernel/src/memory_extraction.rs`

Each extractor is a struct implementing `MemoryExtractor`. They parse the known JSON structure of their tool's output and extract salient facts deterministically.

```rust
// ---------------------------------------------------------------------------
// http-client extractor
// ---------------------------------------------------------------------------
// Tool returns: { "status": 200, "headers": {...}, "body": <json|string>, "latency_ms": 42, "truncated": false }
pub struct HttpClientExtractor;

impl MemoryExtractor for HttpClientExtractor {
    fn tool_name(&self) -> &str {
        "http-client"
    }

    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact> {
        let mut facts = Vec::new();

        let status = result.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
        // Only extract from successful responses with a body
        if !(200..300).contains(&(status as u16 as u64)) {
            return facts;
        }

        let body = match result.get("body") {
            Some(b) => b,
            None => return facts,
        };

        // If the body is a JSON object, summarize its top-level keys
        if let Some(obj) = body.as_object() {
            // Extract structure: list of top-level keys with types
            let key_summary: Vec<String> = obj
                .iter()
                .take(20) // cap to avoid huge summaries
                .map(|(k, v)| {
                    let type_hint = match v {
                        serde_json::Value::Array(a) => format!("array[{}]", a.len()),
                        serde_json::Value::Object(_) => "object".to_string(),
                        serde_json::Value::String(_) => "string".to_string(),
                        serde_json::Value::Number(_) => "number".to_string(),
                        serde_json::Value::Bool(_) => "bool".to_string(),
                        serde_json::Value::Null => "null".to_string(),
                    };
                    format!("{}: {}", k, type_hint)
                })
                .collect();

            if !key_summary.is_empty() {
                facts.push(ExtractedFact {
                    key: format!("http-response-schema:{}", ctx.task_id),
                    content: format!(
                        "HTTP {} response (status {}) returned JSON with fields: {}",
                        ctx.tool_name,
                        status,
                        key_summary.join(", ")
                    ),
                    tags: vec!["http".into(), "api-schema".into(), "auto-extracted".into()],
                });
            }
        }

        // If the body is a string longer than 100 chars, store a truncated summary
        if let Some(text) = body.as_str() {
            if text.len() > 100 {
                let preview = &text[..text.len().min(500)];
                facts.push(ExtractedFact {
                    key: format!("http-response-text:{}", ctx.task_id),
                    content: format!(
                        "HTTP response (status {}) returned text ({} chars): {}...",
                        status,
                        text.len(),
                        preview
                    ),
                    tags: vec!["http".into(), "response-text".into(), "auto-extracted".into()],
                });
            }
        }

        facts
    }
}

// ---------------------------------------------------------------------------
// shell-exec extractor
// ---------------------------------------------------------------------------
// Tool returns: { "stdout": "...", "stderr": "...", "exit_code": 0 }
pub struct ShellExecExtractor;

impl MemoryExtractor for ShellExecExtractor {
    fn tool_name(&self) -> &str {
        "shell-exec"
    }

    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact> {
        let mut facts = Vec::new();

        let exit_code = result
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);

        let stdout = result
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let stderr = result
            .get("stderr")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Only extract from successful commands with meaningful output
        if exit_code != 0 {
            // Failed commands: extract the error for future reference
            if !stderr.is_empty() && stderr.len() > 20 {
                let preview = &stderr[..stderr.len().min(300)];
                facts.push(ExtractedFact {
                    key: format!("shell-error:{}", ctx.task_id),
                    content: format!(
                        "Shell command failed (exit {}): {}",
                        exit_code, preview
                    ),
                    tags: vec![
                        "shell".into(),
                        "error".into(),
                        "auto-extracted".into(),
                    ],
                });
            }
            return facts;
        }

        // Successful: extract stdout summary if non-trivial
        if stdout.len() > 50 {
            let preview = &stdout[..stdout.len().min(500)];
            facts.push(ExtractedFact {
                key: format!("shell-output:{}", ctx.task_id),
                content: format!(
                    "Shell command succeeded (exit 0), output ({} chars): {}",
                    stdout.len(),
                    preview
                ),
                tags: vec!["shell".into(), "output".into(), "auto-extracted".into()],
            });
        }

        facts
    }
}

// ---------------------------------------------------------------------------
// file-reader extractor
// ---------------------------------------------------------------------------
// Tool returns: { "content": "...", "path": "..." } or plain string content
pub struct FileReaderExtractor;

impl MemoryExtractor for FileReaderExtractor {
    fn tool_name(&self) -> &str {
        "file-reader"
    }

    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact> {
        let mut facts = Vec::new();

        let path = result
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let content = result
            .get("content")
            .and_then(|v| v.as_str())
            .or_else(|| result.as_str())
            .unwrap_or("");

        if content.len() < 50 {
            return facts;
        }

        // Store that a file was read and its approximate size
        facts.push(ExtractedFact {
            key: format!("file-read:{}", path),
            content: format!(
                "Read file '{}' ({} chars). Preview: {}",
                path,
                content.len(),
                &content[..content.len().min(300)]
            ),
            tags: vec![
                "file".into(),
                "read".into(),
                "auto-extracted".into(),
            ],
        });

        facts
    }
}

// ---------------------------------------------------------------------------
// data-parser extractor
// ---------------------------------------------------------------------------
// Tool returns parsed structured data (JSON, CSV, TOML)
pub struct DataParserExtractor;

impl MemoryExtractor for DataParserExtractor {
    fn tool_name(&self) -> &str {
        "data-parser"
    }

    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact> {
        let mut facts = Vec::new();

        // If the result is an object, summarize its structure
        if let Some(obj) = result.as_object() {
            let key_count = obj.len();
            if key_count > 0 {
                let keys: Vec<&str> = obj.keys().take(15).map(|k| k.as_str()).collect();
                facts.push(ExtractedFact {
                    key: format!("parsed-data-schema:{}", ctx.task_id),
                    content: format!(
                        "Parsed structured data with {} top-level keys: {}",
                        key_count,
                        keys.join(", ")
                    ),
                    tags: vec![
                        "data".into(),
                        "parsed".into(),
                        "auto-extracted".into(),
                    ],
                });
            }
        }

        // If the result is an array, note its length and first element structure
        if let Some(arr) = result.as_array() {
            if !arr.is_empty() {
                let first_keys = arr[0]
                    .as_object()
                    .map(|obj| {
                        let keys: Vec<&str> = obj.keys().take(10).map(|k| k.as_str()).collect();
                        format!(" with fields: {}", keys.join(", "))
                    })
                    .unwrap_or_default();

                facts.push(ExtractedFact {
                    key: format!("parsed-data-array:{}", ctx.task_id),
                    content: format!(
                        "Parsed array of {} items{}",
                        arr.len(),
                        first_keys
                    ),
                    tags: vec![
                        "data".into(),
                        "parsed".into(),
                        "auto-extracted".into(),
                    ],
                });
            }
        }

        facts
    }
}
```

### 6.3 Implement ExtractionRegistry and MemoryExtractionEngine

**Where:** `crates/agentos-kernel/src/memory_extraction.rs`

```rust
/// Registry mapping tool names to their extractors.
pub struct ExtractionRegistry {
    extractors: HashMap<String, Box<dyn MemoryExtractor>>,
}

impl ExtractionRegistry {
    pub fn new() -> Self {
        Self {
            extractors: HashMap::new(),
        }
    }

    /// Register a per-tool memory extractor.
    pub fn register(&mut self, extractor: Box<dyn MemoryExtractor>) {
        self.extractors
            .insert(extractor.tool_name().to_string(), extractor);
    }

    /// Look up the extractor for a given tool name.
    pub fn get(&self, tool_name: &str) -> Option<&dyn MemoryExtractor> {
        self.extractors.get(tool_name).map(|e| e.as_ref())
    }

    /// Register all built-in core tool extractors.
    pub fn register_defaults(&mut self) {
        self.register(Box::new(HttpClientExtractor));
        self.register(Box::new(ShellExecExtractor));
        self.register(Box::new(FileReaderExtractor));
        self.register(Box::new(DataParserExtractor));
    }
}

/// Configuration for the extraction engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionConfig {
    /// Master switch to enable/disable automatic extraction.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Cosine similarity threshold above which an existing memory is considered
    /// a conflict (i.e., the new fact duplicates or updates it).
    #[serde(default = "default_conflict_threshold")]
    pub conflict_threshold: f32,
    /// Maximum number of facts to extract per tool result.
    #[serde(default = "default_max_facts_per_result")]
    pub max_facts_per_result: usize,
    /// Minimum result JSON string length to bother extracting from.
    #[serde(default = "default_min_result_length")]
    pub min_result_length: usize,
}

fn default_enabled() -> bool {
    true
}
fn default_conflict_threshold() -> f32 {
    0.85
}
fn default_max_facts_per_result() -> usize {
    5
}
fn default_min_result_length() -> usize {
    50
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            conflict_threshold: default_conflict_threshold(),
            max_facts_per_result: default_max_facts_per_result(),
            min_result_length: default_min_result_length(),
        }
    }
}

/// The engine that runs extraction + conflict detection after tool results.
pub struct MemoryExtractionEngine {
    registry: ExtractionRegistry,
    semantic_store: Arc<SemanticStore>,
    config: ExtractionConfig,
}

impl MemoryExtractionEngine {
    pub fn new(
        registry: ExtractionRegistry,
        semantic_store: Arc<SemanticStore>,
        config: ExtractionConfig,
    ) -> Self {
        Self {
            registry,
            semantic_store,
            config,
        }
    }

    /// Process a tool result: extract facts, detect conflicts, apply operations.
    /// Returns the number of facts written (ADD + UPDATE).
    ///
    /// This method is designed to be called from a `tokio::spawn` background
    /// task so it never blocks the agent loop.
    pub async fn process_tool_result(
        &self,
        tool_name: &str,
        result: &serde_json::Value,
        ctx: &ExtractionContext,
    ) -> Result<ExtractionReport, AgentOSError> {
        if !self.config.enabled {
            return Ok(ExtractionReport::default());
        }

        // Skip tiny results
        let result_str = result.to_string();
        if result_str.len() < self.config.min_result_length {
            return Ok(ExtractionReport::default());
        }

        // Look up the extractor for this tool
        let extractor = match self.registry.get(tool_name) {
            Some(e) => e,
            None => {
                // No extractor registered for this tool -- skip silently
                return Ok(ExtractionReport::default());
            }
        };

        // Extract facts
        let mut facts = extractor.extract(result, ctx);
        facts.truncate(self.config.max_facts_per_result);

        if facts.is_empty() {
            return Ok(ExtractionReport::default());
        }

        // Process each fact: conflict detection + operation
        let mut report = ExtractionReport::default();

        for fact in facts {
            let operation = self
                .detect_conflict(&fact, &ctx.agent_id)
                .await?;

            match operation {
                MemoryOperation::Add(f) => {
                    let tag_refs: Vec<&str> = f.tags.iter().map(|s| s.as_str()).collect();
                    self.semantic_store
                        .write(&f.key, &f.content, Some(&ctx.agent_id), &tag_refs)
                        .await?;
                    report.added += 1;
                }
                MemoryOperation::Update {
                    existing_id,
                    existing_key: _,
                    new_content,
                    tags,
                } => {
                    // Delete-then-write to update (SemanticStore has no in-place update)
                    self.semantic_store.delete(&existing_id)?;
                    let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
                    self.semantic_store
                        .write(&fact.key, &new_content, Some(&ctx.agent_id), &tag_refs)
                        .await?;
                    report.updated += 1;
                }
                MemoryOperation::Noop { .. } => {
                    report.skipped += 1;
                }
            }
        }

        Ok(report)
    }

    /// Check if a similar memory already exists. Returns the appropriate operation.
    async fn detect_conflict(
        &self,
        fact: &ExtractedFact,
        agent_id: &AgentID,
    ) -> Result<MemoryOperation, AgentOSError> {
        // Search for semantically similar existing memories
        let results = self
            .semantic_store
            .search(
                &fact.content,
                Some(agent_id),
                3, // top 3 candidates
                self.config.conflict_threshold,
            )
            .await?;

        if results.is_empty() {
            // No similar memory -- add as new
            return Ok(MemoryOperation::Add(fact.clone()));
        }

        let top = &results[0];

        // The search already filters by min_score = conflict_threshold, so
        // anything returned is above the threshold.
        //
        // Heuristic: if the existing content is a strict substring of the new
        // content (or vice versa), treat as UPDATE (the new fact is more
        // detailed). Otherwise, if cosine > 0.95 treat as NOOP (essentially
        // the same fact). Between 0.85-0.95 treat as UPDATE (related but
        // different enough to refine).
        if top.semantic_score > 0.95 {
            Ok(MemoryOperation::Noop {
                reason: format!(
                    "Existing memory '{}' is near-identical (cosine {:.3})",
                    top.entry.key, top.semantic_score
                ),
            })
        } else {
            // cosine between conflict_threshold and 0.95 -- update
            Ok(MemoryOperation::Update {
                existing_id: top.entry.id.clone(),
                existing_key: top.entry.key.clone(),
                new_content: fact.content.clone(),
                tags: fact.tags.clone(),
            })
        }
    }
}

/// Summary of what the extraction engine did for a single tool result.
#[derive(Debug, Default, Clone)]
pub struct ExtractionReport {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
}

impl std::fmt::Display for ExtractionReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "+{} ~{} ={} (add/update/skip)",
            self.added, self.updated, self.skipped
        )
    }
}
```

### 6.4 Add config section

**Where:** `crates/agentos-kernel/src/config.rs`

Add `ExtractionConfig` to `MemorySettings`:

```rust
// In config.rs, modify MemorySettings:
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemorySettings {
    #[serde(default = "default_model_cache_dir")]
    pub model_cache_dir: String,
    #[serde(default)]
    pub extraction: crate::memory_extraction::ExtractionConfig,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            model_cache_dir: default_model_cache_dir(),
            extraction: crate::memory_extraction::ExtractionConfig::default(),
        }
    }
}
```

**Where:** `config/default.toml`

Append:

```toml
[memory.extraction]
enabled = true
conflict_threshold = 0.85
max_facts_per_result = 5
min_result_length = 50
```

### 6.5 Wire into the kernel

**Where:** `crates/agentos-kernel/src/kernel.rs`

Add `memory_extraction` field to `Kernel` struct and initialize at boot:

```rust
// In kernel.rs, add field to Kernel struct:
pub memory_extraction: Arc<crate::memory_extraction::MemoryExtractionEngine>,

// In Kernel::boot(), after semantic store initialization, before the final
// Kernel { ... } struct literal:

// Build semantic store for extraction engine
let semantic_store = Arc::new(
    agentos_memory::SemanticStore::open_with_cache_dir(&data_dir, &model_cache_dir)?
);

let mut extraction_registry = crate::memory_extraction::ExtractionRegistry::new();
extraction_registry.register_defaults();
let memory_extraction = Arc::new(crate::memory_extraction::MemoryExtractionEngine::new(
    extraction_registry,
    semantic_store,
    config.memory.extraction.clone(),
));

// Add to Kernel struct literal:
// memory_extraction,
```

**Where:** `crates/agentos-kernel/src/lib.rs`

Add module declaration:

```rust
pub mod memory_extraction;
```

### 6.6 Hook into task_executor after tool results

**Where:** `crates/agentos-kernel/src/task_executor.rs`

After the existing block that pushes tool results to context (around line 1192-1194, after `push_tool_result` and before the episodic memory record), spawn the extraction as a non-blocking background task:

```rust
// After: self.context_manager.push_tool_result(&task.id, &tool_call.tool_name, &tainted_result).await.ok();
// Add:

// --- Structured memory extraction (Phase 6) ---
// Fire-and-forget: extract facts from the tool result and write to semantic memory.
// Uses per-tool extractors, not LLM calls -- cheap and non-blocking.
{
    let extraction_engine = self.memory_extraction.clone();
    let tool_name = tool_call.tool_name.clone();
    let extraction_result = context_result.clone();
    let extraction_ctx = crate::memory_extraction::ExtractionContext {
        tool_name: tool_call.tool_name.clone(),
        agent_id: task.agent_id,
        task_id: task.id,
    };
    tokio::spawn(async move {
        match extraction_engine
            .process_tool_result(&tool_name, &extraction_result, &extraction_ctx)
            .await
        {
            Ok(report) => {
                if report.added > 0 || report.updated > 0 {
                    tracing::debug!(
                        tool = %tool_name,
                        "Memory extraction: {}",
                        report
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    tool = %tool_name,
                    error = %e,
                    "Memory extraction failed"
                );
            }
        }
    });
}
```

### 6.7 Write tests

**Where:** `crates/agentos-kernel/src/memory_extraction.rs` (inline `#[cfg(test)]` module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- Unit tests for individual extractors ---

    #[test]
    fn test_http_extractor_success_json() {
        let extractor = HttpClientExtractor;
        let result = json!({
            "status": 200,
            "headers": { "content-type": "application/json" },
            "body": {
                "users": [{"id": 1, "name": "Alice"}],
                "total": 42,
                "page": 1
            },
            "latency_ms": 150,
            "truncated": false
        });
        let ctx = ExtractionContext {
            tool_name: "http-client".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert!(!facts.is_empty(), "Should extract at least one fact from JSON response");
        assert!(
            facts[0].content.contains("users"),
            "Fact should mention the top-level keys"
        );
        assert!(
            facts[0].tags.contains(&"auto-extracted".to_string()),
            "Facts should be tagged as auto-extracted"
        );
    }

    #[test]
    fn test_http_extractor_error_status_skipped() {
        let extractor = HttpClientExtractor;
        let result = json!({
            "status": 404,
            "headers": {},
            "body": "Not Found",
            "latency_ms": 50,
            "truncated": false
        });
        let ctx = ExtractionContext {
            tool_name: "http-client".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert!(facts.is_empty(), "Should not extract from error responses");
    }

    #[test]
    fn test_shell_extractor_success() {
        let extractor = ShellExecExtractor;
        let result = json!({
            "stdout": "total 128\ndrwxr-xr-x 5 user staff 160 Mar 12 10:00 src\n-rw-r--r-- 1 user staff 4096 Mar 12 09:00 Cargo.toml\nmore lines here to exceed the 50 char minimum",
            "stderr": "",
            "exit_code": 0
        });
        let ctx = ExtractionContext {
            tool_name: "shell-exec".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert_eq!(facts.len(), 1, "Should extract one fact from successful shell output");
        assert!(facts[0].content.contains("exit 0"));
    }

    #[test]
    fn test_shell_extractor_failure() {
        let extractor = ShellExecExtractor;
        let result = json!({
            "stdout": "",
            "stderr": "error: cannot find crate `nonexistent` -- this is a longer error message to exceed minimum",
            "exit_code": 1
        });
        let ctx = ExtractionContext {
            tool_name: "shell-exec".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert_eq!(facts.len(), 1, "Should extract error fact");
        assert!(facts[0].content.contains("failed"));
        assert!(facts[0].tags.contains(&"error".to_string()));
    }

    #[test]
    fn test_shell_extractor_short_output_skipped() {
        let extractor = ShellExecExtractor;
        let result = json!({
            "stdout": "ok",
            "stderr": "",
            "exit_code": 0
        });
        let ctx = ExtractionContext {
            tool_name: "shell-exec".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert!(facts.is_empty(), "Should skip short outputs");
    }

    #[test]
    fn test_file_reader_extractor() {
        let extractor = FileReaderExtractor;
        let result = json!({
            "content": "This is a long file content that contains important configuration settings for the deployment pipeline and exceeds fifty characters easily.",
            "path": "/data/config.yaml"
        });
        let ctx = ExtractionContext {
            tool_name: "file-reader".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert_eq!(facts.len(), 1);
        assert!(facts[0].key.contains("config.yaml"));
        assert!(facts[0].content.contains("/data/config.yaml"));
    }

    #[test]
    fn test_file_reader_short_content_skipped() {
        let extractor = FileReaderExtractor;
        let result = json!({
            "content": "short",
            "path": "/data/tiny.txt"
        });
        let ctx = ExtractionContext {
            tool_name: "file-reader".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert!(facts.is_empty());
    }

    #[test]
    fn test_data_parser_object() {
        let extractor = DataParserExtractor;
        let result = json!({
            "name": "AgentOS",
            "version": "3.0",
            "features": ["memory", "tools"]
        });
        let ctx = ExtractionContext {
            tool_name: "data-parser".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert_eq!(facts.len(), 1);
        assert!(facts[0].content.contains("3 top-level keys"));
    }

    #[test]
    fn test_data_parser_array() {
        let extractor = DataParserExtractor;
        let result = json!([
            {"id": 1, "name": "Alice"},
            {"id": 2, "name": "Bob"}
        ]);
        let ctx = ExtractionContext {
            tool_name: "data-parser".into(),
            agent_id: AgentID::new(),
            task_id: agentos_types::TaskID::new(),
        };

        let facts = extractor.extract(&result, &ctx);
        assert_eq!(facts.len(), 1);
        assert!(facts[0].content.contains("2 items"));
        assert!(facts[0].content.contains("id"));
    }

    // --- Registry tests ---

    #[test]
    fn test_registry_defaults() {
        let mut registry = ExtractionRegistry::new();
        registry.register_defaults();

        assert!(registry.get("http-client").is_some());
        assert!(registry.get("shell-exec").is_some());
        assert!(registry.get("file-reader").is_some());
        assert!(registry.get("data-parser").is_some());
        assert!(registry.get("nonexistent-tool").is_none());
    }

    #[test]
    fn test_registry_tool_name_matches() {
        let mut registry = ExtractionRegistry::new();
        registry.register_defaults();

        let http = registry.get("http-client").unwrap();
        assert_eq!(http.tool_name(), "http-client");
    }

    // --- Config tests ---

    #[test]
    fn test_extraction_config_defaults() {
        let config = ExtractionConfig::default();
        assert!(config.enabled);
        assert!((config.conflict_threshold - 0.85).abs() < f32::EPSILON);
        assert_eq!(config.max_facts_per_result, 5);
        assert_eq!(config.min_result_length, 50);
    }

    #[test]
    fn test_extraction_config_deserialize() {
        let toml_str = r#"
            enabled = false
            conflict_threshold = 0.90
            max_facts_per_result = 3
            min_result_length = 100
        "#;
        let config: ExtractionConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.enabled);
        assert!((config.conflict_threshold - 0.90).abs() < f32::EPSILON);
        assert_eq!(config.max_facts_per_result, 3);
        assert_eq!(config.min_result_length, 100);
    }

    // --- Report display ---

    #[test]
    fn test_report_display() {
        let report = ExtractionReport {
            added: 2,
            updated: 1,
            skipped: 3,
        };
        let s = format!("{}", report);
        assert_eq!(s, "+2 ~1 =3 (add/update/skip)");
    }
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/memory_extraction.rs` | **New** -- `MemoryExtractor` trait, 4 core extractors, `ExtractionRegistry`, `MemoryExtractionEngine`, `ExtractionConfig`, conflict detection, tests |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod memory_extraction;` (1 line) |
| `crates/agentos-kernel/src/config.rs` | Add `extraction: ExtractionConfig` field to `MemorySettings`, update `Default` impl |
| `crates/agentos-kernel/src/kernel.rs` | Add `memory_extraction: Arc<MemoryExtractionEngine>` field, initialize at boot with `ExtractionRegistry::register_defaults()` |
| `crates/agentos-kernel/src/task_executor.rs` | Add `tokio::spawn` block after `push_tool_result` (~15 lines) to fire extraction as background task |
| `config/default.toml` | Add `[memory.extraction]` section (4 lines) |

---

## Dependencies

- **Requires:** Phase 1 (episodic auto-write established), Phase 2 (semantic store operational with search), Phase 5 (adaptive retrieval gate -- ensures extraction doesn't interfere with retrieval path)
- **Blocks:** Phase 7 (consolidation pathways -- operates on the enriched semantic store that extraction populates)

---

## Test Plan

### Unit tests (in `memory_extraction.rs`)

| Test | Assertion |
|------|-----------|
| `test_http_extractor_success_json` | Extracts facts from 200 JSON response with top-level keys |
| `test_http_extractor_error_status_skipped` | Returns empty for 404/500 responses |
| `test_shell_extractor_success` | Extracts output summary from exit-0 commands |
| `test_shell_extractor_failure` | Extracts error fact from non-zero exit codes |
| `test_shell_extractor_short_output_skipped` | Returns empty for output < 50 chars |
| `test_file_reader_extractor` | Extracts path and content preview |
| `test_file_reader_short_content_skipped` | Returns empty for content < 50 chars |
| `test_data_parser_object` | Extracts key count and key names from JSON object |
| `test_data_parser_array` | Extracts item count and field names from JSON array |
| `test_registry_defaults` | All 4 core extractors registered |
| `test_registry_tool_name_matches` | Extractor `.tool_name()` matches registered key |
| `test_extraction_config_defaults` | Default config has `enabled=true`, threshold 0.85 |
| `test_extraction_config_deserialize` | Config round-trips through TOML |
| `test_report_display` | `ExtractionReport` formats correctly |

### Integration tests (require `SemanticStore` with embedder)

| Test | Assertion |
|------|-----------|
| `process_tool_result` with no extractor | Returns empty `ExtractionReport` (0/0/0) |
| `process_tool_result` with disabled config | Returns empty `ExtractionReport` |
| `process_tool_result` with short result | Returns empty `ExtractionReport` |
| `process_tool_result` ADD path | Fact is written to `SemanticStore`, `report.added == 1` |
| `process_tool_result` NOOP path | Duplicate fact (cosine > 0.95) skipped, `report.skipped == 1` |
| `process_tool_result` UPDATE path | Similar-but-different fact replaces existing, `report.updated == 1`, old entry deleted |

---

## Verification

```bash
# 1. Build the workspace (must compile cleanly)
cargo build --workspace

# 2. Run memory_extraction unit tests
cargo test -p agentos-kernel -- memory_extraction --nocapture

# 3. Run full kernel test suite (ensure no regressions)
cargo test -p agentos-kernel

# 4. Clippy lint check
cargo clippy --workspace -- -D warnings

# 5. Format check
cargo fmt --all -- --check

# 6. Verify config deserialization with new section
# (the kernel boot test in tests/kernel_boot_test.rs exercises config loading)
cargo test -p agentos-cli -- kernel_boot --nocapture
```

---

## Key Design Decisions

1. **No LLM calls for extraction.** Per-tool extractors are deterministic Rust functions that parse known JSON schemas. This eliminates the cost-doubling problem of the original Mem0-style pipeline and removes latency from the extraction path.

2. **Per-tool extractors, not generic parsing.** Each core tool has a dedicated `MemoryExtractor` implementation that understands its output schema. This produces higher-quality facts than a generic JSON-walker because the extractor knows which fields are salient (e.g., `status` in HTTP, `exit_code` in shell).

3. **Conflict detection via cosine similarity, not LLM.** The `SemanticStore.search()` already computes cosine similarity with a min_score threshold. We use 0.85 as the conflict boundary and 0.95 as the near-duplicate boundary. No LLM call needed to decide ADD vs UPDATE vs NOOP.

4. **No DELETE operation.** Only agents should delete their own memories via the `memory-write` tool. Automated deletion risks losing valuable context that the agent explicitly stored.

5. **Background task, fire-and-forget.** Extraction runs in a `tokio::spawn` task after `push_tool_result`. It never blocks the agent loop. If it fails, the error is logged and the agent continues unaffected.

6. **Extensible registry.** Third-party tools can register their own `MemoryExtractor` implementations. The `ExtractionRegistry` is populated at boot and can be extended at tool install time.

---

## Related

- [[Memory Context Architecture Plan]] -- master plan
- [[05-adaptive-retrieval-gate]] -- previous phase (ensures extraction doesn't interfere with retrieval)
- [[07-consolidation-pathways]] -- next phase (operates on the enriched semantic store)
- [[Memory Context Data Flow]] -- data flow diagram showing extraction in the pipeline
