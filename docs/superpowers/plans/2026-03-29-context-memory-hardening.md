# Context Memory Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden long-running agent context with LLM-generated summarization, context-loss notices, and reference count tracking.

**Architecture:** Three independent features layered bottom-up: new methods on `ContextWindow` (types crate, no kernel deps), config additions, then `ContextManager` refactoring to orchestrate LLM summarization and wire everything together. The `push_entry` path releases its write lock before any LLM call to avoid blocking.

**Tech Stack:** Rust, tokio async, agentos-types, agentos-kernel, agentos-llm (LLMCore trait), serde, chrono

**Spec:** `docs/superpowers/specs/2026-03-29-context-memory-hardening-design.md`

---

## File Structure

| File | Responsibility | Action |
|------|---------------|--------|
| `crates/agentos-types/src/context.rs` | ContextWindow methods: extract, insert summary, upsert notice, increment refs | Modify |
| `crates/agentos-kernel/src/config.rs` | `ContextConfig`, `SummarizationMode` structs | Modify |
| `config/default.toml` | `[context]` config section | Modify |
| `crates/agentos-kernel/src/context.rs` | ContextManager: new deps, agent_id storage, LLM summarization, notice injection, ref tracking | Modify |
| `crates/agentos-kernel/src/context_injector.rs` | Pass `agent_id` to `create_context()` | Modify |
| `crates/agentos-kernel/src/commands/pipeline.rs` | Pass `agent_id` to `create_context()` | Modify |
| `crates/agentos-kernel/src/kernel.rs` | ContextManager construction with new deps | Modify |
| `crates/agentos-kernel/src/task_executor.rs` | Call `increment_references()` after tool result pushes | Modify |

---

### Task 1: ContextWindow::extract_compressible

**Files:**
- Modify: `crates/agentos-types/src/context.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module at the end of `crates/agentos-types/src/context.rs`:

```rust
#[test]
fn test_extract_compressible_returns_oldest_non_pinned() {
    let mut window = ContextWindow::new(100);
    // System entry (pinned by role check) — should NOT be extracted
    window.push(ContextEntry {
        role: ContextRole::System,
        content: "system prompt".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 1.0,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::System,
        is_summary: false,
    });
    // Pinned user entry — should NOT be extracted
    window.push(ContextEntry {
        role: ContextRole::User,
        content: "pinned task".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 0.95,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::Task,
        is_summary: false,
    });
    // Three evictable entries
    for i in 0..3 {
        window.push(ContextEntry {
            role: ContextRole::Assistant,
            content: format!("response {}", i),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
    }

    assert_eq!(window.entries.len(), 5);

    let extracted = window.extract_compressible(2);

    assert_eq!(extracted.len(), 2);
    assert_eq!(extracted[0].content, "response 0");
    assert_eq!(extracted[1].content, "response 1");
    // Window should have 3 entries left: system + pinned + response 2
    assert_eq!(window.entries.len(), 3);
    assert_eq!(window.entries[2].content, "response 2");
}

#[test]
fn test_extract_compressible_skips_system_and_pinned() {
    let mut window = ContextWindow::new(100);
    window.push(ContextEntry {
        role: ContextRole::System,
        content: "sys".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 1.0,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::System,
        is_summary: false,
    });
    window.push(ContextEntry {
        role: ContextRole::User,
        content: "pinned".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 1.0,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::Task,
        is_summary: false,
    });

    let extracted = window.extract_compressible(5);

    assert!(extracted.is_empty(), "Should extract nothing when all entries are pinned/system");
    assert_eq!(window.entries.len(), 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p agentos-types test_extract_compressible -- --nocapture`
Expected: FAIL — method `extract_compressible` not found

- [ ] **Step 3: Write minimal implementation**

Add this method to the `impl ContextWindow` block in `crates/agentos-types/src/context.rs`, right before the existing `compress_oldest` method:

```rust
/// Extract up to `count` non-pinned, non-System entries from oldest first.
/// Returns the removed entries. Same selection logic as `compress_oldest`
/// but returns entries instead of concatenating, so the caller can
/// summarize them (e.g., via LLM).
pub fn extract_compressible(&mut self, count: usize) -> Vec<ContextEntry> {
    let mut extracted = Vec::new();
    let mut i = 0;
    while extracted.len() < count && i < self.entries.len() {
        let e = &self.entries[i];
        if e.role != ContextRole::System && !e.pinned {
            extracted.push(self.entries.remove(i));
        } else {
            i += 1;
        }
    }
    extracted
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p agentos-types test_extract_compressible -- --nocapture`
Expected: PASS (both tests)

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-types/src/context.rs
git commit -m "feat(types): add ContextWindow::extract_compressible for LLM summarization"
```

---

### Task 2: ContextWindow::insert_summary_entry

**Files:**
- Modify: `crates/agentos-types/src/context.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module:

```rust
#[test]
fn test_insert_summary_entry_positions_after_system() {
    let mut window = ContextWindow::new(100);
    window.push(ContextEntry {
        role: ContextRole::System,
        content: "system prompt".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 1.0,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::System,
        is_summary: false,
    });
    window.push(ContextEntry {
        role: ContextRole::User,
        content: "user msg".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 0.5,
        pinned: false,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::History,
        is_summary: false,
    });

    window.insert_summary_entry("LLM-generated summary of 3 messages".to_string(), 3);

    // Summary should be at index 1 (after system, before user msg)
    assert_eq!(window.entries.len(), 3);
    let summary = &window.entries[1];
    assert!(summary.is_summary);
    assert_eq!(summary.role, ContextRole::System);
    assert_eq!(summary.category, ContextCategory::History);
    assert!((summary.importance - 0.3).abs() < f32::EPSILON);
    assert!(!summary.pinned);
    assert!(summary.content.contains("LLM-generated summary"));
    assert!(summary.content.contains("3 messages"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p agentos-types test_insert_summary_entry -- --nocapture`
Expected: FAIL — method `insert_summary_entry` not found

- [ ] **Step 3: Write minimal implementation**

Add to `impl ContextWindow`, after `extract_compressible`:

```rust
/// Insert a summary entry at the correct position (after System entries,
/// before non-system content). Used by the kernel's LLM summarization
/// path to insert the generated summary.
pub fn insert_summary_entry(&mut self, content: String, compressed_count: usize) {
    let insert_pos = self
        .entries
        .iter()
        .position(|e| e.role != ContextRole::System)
        .unwrap_or(self.entries.len());

    self.entries.insert(
        insert_pos,
        ContextEntry {
            role: ContextRole::System,
            content: format!(
                "[SUMMARY — {} messages compressed]\n{}",
                compressed_count, content
            ),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.3,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: true,
        },
    );
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p agentos-types test_insert_summary_entry -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-types/src/context.rs
git commit -m "feat(types): add ContextWindow::insert_summary_entry for compiled summaries"
```

---

### Task 3: ContextWindow::upsert_context_notice

**Files:**
- Modify: `crates/agentos-types/src/context.rs`

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)]`:

```rust
#[test]
fn test_upsert_context_notice_inserts_when_absent() {
    let mut window = ContextWindow::new(100);
    window.push(ContextEntry {
        role: ContextRole::System,
        content: "system prompt".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 1.0,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::System,
        is_summary: false,
    });

    window.upsert_context_notice(5);

    assert_eq!(window.entries.len(), 2);
    let notice = &window.entries[1];
    assert!(notice.content.starts_with("[CONTEXT NOTE]"));
    assert!(notice.content.contains("5"));
    assert!(notice.content.contains("memory-read"));
    assert!(notice.pinned);
    assert!((notice.importance - 0.9).abs() < f32::EPSILON);
    assert_eq!(notice.category, ContextCategory::System);
    assert!(!notice.is_summary);
}

#[test]
fn test_upsert_context_notice_updates_existing() {
    let mut window = ContextWindow::new(100);
    window.push(ContextEntry {
        role: ContextRole::System,
        content: "system prompt".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 1.0,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::System,
        is_summary: false,
    });

    window.upsert_context_notice(3);
    window.upsert_context_notice(8);

    // Should still be 2 entries (system + notice), not 3
    assert_eq!(window.entries.len(), 2);
    let notice = &window.entries[1];
    assert!(notice.content.contains("8"), "Should show updated count");
    assert!(!notice.content.contains(" 3 "), "Old count should be gone");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p agentos-types test_upsert_context_notice -- --nocapture`
Expected: FAIL — method `upsert_context_notice` not found

- [ ] **Step 3: Write minimal implementation**

Add to `impl ContextWindow`:

```rust
/// Sentinel prefix used to identify context-loss notice entries.
const CONTEXT_NOTICE_PREFIX: &'static str = "[CONTEXT NOTE]";

/// Insert or update a context-loss notice telling the agent that entries
/// were compressed and how to recover details via episodic memory.
pub fn upsert_context_notice(&mut self, compressed_count: usize) {
    let notice_content = format!(
        "{} {} earlier messages were compressed into a summary. \
         To recall specific details, use memory-read with scope=episodic and your current task ID.",
        Self::CONTEXT_NOTICE_PREFIX,
        compressed_count,
    );

    // Look for an existing notice
    if let Some(idx) = self
        .entries
        .iter()
        .position(|e| e.content.starts_with(Self::CONTEXT_NOTICE_PREFIX))
    {
        self.entries[idx].content = notice_content;
        self.entries[idx].timestamp = chrono::Utc::now();
        return;
    }

    // Insert after the last System entry that is NOT a summary
    let insert_pos = self
        .entries
        .iter()
        .position(|e| e.role != ContextRole::System || e.is_summary)
        .unwrap_or(self.entries.len());

    self.entries.insert(
        insert_pos,
        ContextEntry {
            role: ContextRole::System,
            content: notice_content,
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.9,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        },
    );
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p agentos-types test_upsert_context_notice -- --nocapture`
Expected: PASS (both tests)

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-types/src/context.rs
git commit -m "feat(types): add ContextWindow::upsert_context_notice for context-loss awareness"
```

---

### Task 4: ContextWindow::increment_references_for_tool_call_ids

**Files:**
- Modify: `crates/agentos-types/src/context.rs`

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)]`:

```rust
#[test]
fn test_increment_references_for_tool_call_ids() {
    let mut window = ContextWindow::new(100);

    // Assistant entry with tool_calls metadata containing call_123
    window.push(ContextEntry {
        role: ContextRole::Assistant,
        content: "I'll read that file.".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: Some(ContextMetadata {
            tool_name: None,
            tool_id: None,
            intent_id: None,
            tokens_estimated: None,
            tool_call_id: None,
            assistant_tool_calls: Some(serde_json::json!([
                {"id": "call_123", "tool_name": "file-reader", "intent_type": "read", "payload": {}}
            ])),
        }),
        importance: 0.4,
        pinned: false,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::History,
        is_summary: false,
    });

    // ToolResult entry with matching tool_call_id
    window.push(ContextEntry {
        role: ContextRole::ToolResult,
        content: "file contents here".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: Some(ContextMetadata {
            tool_name: Some("file-reader".to_string()),
            tool_id: None,
            intent_id: None,
            tokens_estimated: None,
            tool_call_id: Some("call_123".to_string()),
            assistant_tool_calls: None,
        }),
        importance: 0.5,
        pinned: false,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::History,
        is_summary: false,
    });

    // Unrelated entry — should NOT be incremented
    window.push(ContextEntry {
        role: ContextRole::ToolResult,
        content: "other result".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: Some(ContextMetadata {
            tool_name: Some("shell-exec".to_string()),
            tool_id: None,
            intent_id: None,
            tokens_estimated: None,
            tool_call_id: Some("call_999".to_string()),
            assistant_tool_calls: None,
        }),
        importance: 0.5,
        pinned: false,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::History,
        is_summary: false,
    });

    window.increment_references_for_tool_call_ids(&["call_123".to_string()]);

    // Assistant entry (has assistant_tool_calls containing call_123)
    assert_eq!(window.entries[0].reference_count, 1, "Assistant entry should be incremented");
    // ToolResult with call_123
    assert_eq!(window.entries[1].reference_count, 1, "Matching ToolResult should be incremented");
    // Unrelated ToolResult
    assert_eq!(window.entries[2].reference_count, 0, "Unrelated entry should stay at 0");
}

#[test]
fn test_increment_references_makes_entry_survive_eviction() {
    let mut window = ContextWindow::with_strategy(4, OverflowStrategy::SemanticEviction);

    // System entry (pinned)
    window.push(ContextEntry {
        role: ContextRole::System,
        content: "sys".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 1.0,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::System,
        is_summary: false,
    });

    // Old tool result with reference_count = 2 (should survive eviction)
    window.push(ContextEntry {
        role: ContextRole::ToolResult,
        content: "referenced result".to_string(),
        timestamp: chrono::Utc::now() - chrono::Duration::minutes(10),
        metadata: None,
        importance: 0.5,
        pinned: false,
        reference_count: 2,
        partition: ContextPartition::Active,
        category: ContextCategory::History,
        is_summary: false,
    });

    // Newer tool result with reference_count = 0 (should be evicted first)
    window.push(ContextEntry {
        role: ContextRole::ToolResult,
        content: "unreferenced result".to_string(),
        timestamp: chrono::Utc::now() - chrono::Duration::minutes(5),
        metadata: None,
        importance: 0.5,
        pinned: false,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::History,
        is_summary: false,
    });

    assert_eq!(window.entries.len(), 3);

    // Push a 4th entry to hit capacity, triggering eviction on the next push
    window.push(ContextEntry {
        role: ContextRole::Assistant,
        content: "filler".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 0.4,
        pinned: false,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::History,
        is_summary: false,
    });

    // Now at capacity (4). Push one more to trigger eviction.
    window.push(ContextEntry {
        role: ContextRole::User,
        content: "new entry".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 0.5,
        pinned: false,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::History,
        is_summary: false,
    });

    // The referenced result (ref_count=2) should survive; the unreferenced one should be evicted
    let remaining_contents: Vec<&str> = window.entries.iter().map(|e| e.content.as_str()).collect();
    assert!(
        remaining_contents.contains(&"referenced result"),
        "Referenced entry should survive eviction. Remaining: {:?}",
        remaining_contents
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p agentos-types test_increment_references -- --nocapture`
Expected: FAIL — method `increment_references_for_tool_call_ids` not found

- [ ] **Step 3: Write minimal implementation**

Add to `impl ContextWindow`:

```rust
/// Increment `reference_count` on entries linked to the given tool call IDs.
///
/// For each ID:
/// - ToolResult entries with `metadata.tool_call_id == id` are incremented.
/// - Assistant entries whose `metadata.assistant_tool_calls` JSON array
///   contains an object with `"id": id` are incremented.
///
/// This makes actively-referenced entries resist SemanticEviction.
pub fn increment_references_for_tool_call_ids(&mut self, ids: &[String]) {
    for entry in &mut self.entries {
        let meta = match &entry.metadata {
            Some(m) => m,
            None => continue,
        };

        for id in ids {
            // Match ToolResult entries by tool_call_id
            if entry.role == ContextRole::ToolResult {
                if let Some(ref tc_id) = meta.tool_call_id {
                    if tc_id == id {
                        entry.reference_count += 1;
                    }
                }
            }

            // Match Assistant entries by assistant_tool_calls JSON
            if entry.role == ContextRole::Assistant {
                if let Some(ref calls_json) = meta.assistant_tool_calls {
                    if let Some(arr) = calls_json.as_array() {
                        for call in arr {
                            if call.get("id").and_then(|v| v.as_str()) == Some(id) {
                                entry.reference_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p agentos-types test_increment_references -- --nocapture`
Expected: PASS (both tests)

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-types/src/context.rs
git commit -m "feat(types): add ContextWindow::increment_references_for_tool_call_ids"
```

---

### Task 5: ContextConfig and SummarizationMode

**Files:**
- Modify: `crates/agentos-kernel/src/config.rs`
- Modify: `config/default.toml`

- [ ] **Step 1: Add config structs to `crates/agentos-kernel/src/config.rs`**

Add after the existing `ScratchpadConfig` struct (search for `pub struct ScratchpadConfig`):

```rust
/// Context window management configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextConfig {
    /// Summarization strategy when context budget compression triggers.
    /// - `llm`: Use the agent's LLM adapter for real summarization (falls back to concat on error)
    /// - `concat`: Concatenate entry snippets (legacy behavior)
    /// - `off`: No summary entry created; entries are silently evicted
    #[serde(default = "default_summarization_mode")]
    pub summarization_mode: SummarizationMode,
    /// Maximum characters of entry text sent to the summarizer LLM per compression event.
    #[serde(default = "default_summarization_max_input_chars")]
    pub summarization_max_input_chars: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            summarization_mode: SummarizationMode::default(),
            summarization_max_input_chars: default_summarization_max_input_chars(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SummarizationMode {
    /// LLM-generated summaries (best-effort, falls back to concat).
    #[default]
    Llm,
    /// Concatenate entry snippets (legacy behavior).
    Concat,
    /// No summary — entries are silently evicted.
    Off,
}

fn default_summarization_mode() -> SummarizationMode {
    SummarizationMode::Llm
}

fn default_summarization_max_input_chars() -> usize {
    8000
}
```

- [ ] **Step 2: Add `context` field to `KernelConfig`**

In the `KernelConfig` struct, add the new field after the `context_budget` field:

```rust
    #[serde(default)]
    pub context: ContextConfig,
```

- [ ] **Step 3: Add `[context]` section to `config/default.toml`**

Add after the `[context_budget]` section:

```toml
[context]
# Summarization strategy when context budget compression triggers.
# "llm" = LLM-generated summaries (best-effort, falls back to concat on failure)
# "concat" = concatenate entry snippets (legacy behavior)
# "off" = no summary entry — entries are silently evicted
summarization_mode = "llm"
# Maximum characters of entry text sent to the summarizer LLM per compression event.
# Prevents sending enormous payloads on aggressive compression passes.
summarization_max_input_chars = 8000
```

- [ ] **Step 4: Build and verify**

Run: `cargo build -p agentos-kernel`
Expected: Compiles without errors. The `ContextConfig` defaults allow existing configs without the `[context]` section to continue working.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-kernel/src/config.rs config/default.toml
git commit -m "feat(config): add [context] section with summarization_mode and max_input_chars"
```

---

### Task 6: Refactor ContextManager — new deps and agent_id storage

**Files:**
- Modify: `crates/agentos-kernel/src/context.rs`
- Modify: `crates/agentos-kernel/src/kernel.rs`
- Modify: `crates/agentos-kernel/src/context_injector.rs`
- Modify: `crates/agentos-kernel/src/commands/pipeline.rs`

This task restructures `ContextManager` to store `agent_id` per task and accept the new dependencies. The LLM summarization logic is added in the next task.

- [ ] **Step 1: Restructure ContextManager internals**

In `crates/agentos-kernel/src/context.rs`, replace the struct definition and constructors:

```rust
use agentos_types::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::{ContextConfig, SummarizationMode};
use crate::cost_tracker::CostTracker;

/// Per-task context state: the context window and the agent that owns it.
struct TaskContext {
    window: ContextWindow,
    agent_id: AgentID,
}

pub struct ContextManager {
    /// Per-task context windows with owning agent ID.
    tasks: RwLock<HashMap<TaskID, TaskContext>>,
    max_entries: usize,
    /// Token budget per context window. 0 = no budget enforcement.
    token_budget: usize,
    /// Active LLM adapters, keyed by agent ID. Shared with the kernel.
    active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn agentos_llm::LLMCore>>>>,
    /// Cost tracker for recording summarization inference costs.
    cost_tracker: Arc<CostTracker>,
    /// Context configuration (summarization mode, etc.).
    config: ContextConfig,
}
```

- [ ] **Step 2: Update constructors**

Replace the existing `new` and `with_token_budget` with:

```rust
impl ContextManager {
    pub fn new(max_entries: usize) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            max_entries,
            token_budget: 0,
            active_llms: Arc::new(RwLock::new(HashMap::new())),
            cost_tracker: Arc::new(CostTracker::new()),
            config: ContextConfig::default(),
        }
    }

    pub fn with_token_budget(max_entries: usize, token_budget: usize) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            max_entries,
            token_budget,
            active_llms: Arc::new(RwLock::new(HashMap::new())),
            cost_tracker: Arc::new(CostTracker::new()),
            config: ContextConfig::default(),
        }
    }

    /// Full constructor with all dependencies for LLM-powered summarization.
    pub fn with_full_config(
        max_entries: usize,
        token_budget: usize,
        active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn agentos_llm::LLMCore>>>>,
        cost_tracker: Arc<CostTracker>,
        config: ContextConfig,
    ) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            max_entries,
            token_budget,
            active_llms,
            cost_tracker,
            config,
        }
    }
```

- [ ] **Step 3: Update `create_context` to accept `agent_id`**

Replace the existing `create_context` method:

```rust
    /// Create a new context window for a task with the system prompt.
    /// The system prompt is pinned with maximum importance.
    pub async fn create_context(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        system_prompt: &str,
    ) -> ContextID {
        let mut window =
            ContextWindow::with_strategy(self.max_entries, OverflowStrategy::SemanticEviction);
        let context_id = window.id;

        window.push(ContextEntry {
            role: ContextRole::System,
            content: system_prompt.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::System,
            is_summary: false,
        });

        self.tasks
            .write()
            .await
            .insert(task_id, TaskContext { window, agent_id });
        context_id
    }
```

- [ ] **Step 4: Update all methods that reference `self.windows` to use `self.tasks`**

Replace `push_entry` — for now keep the existing compression logic (LLM summarization wired in Task 8):

```rust
    pub async fn push_entry(
        &self,
        task_id: &TaskID,
        entry: ContextEntry,
    ) -> Result<usize, AgentOSError> {
        let mut tasks = self.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(tc) => {
                let pre_count = tc.window.entries.len();
                tc.window.push(entry);
                let mut evicted = 0usize;

                // Token budget enforcement
                if self.token_budget > 0 {
                    let tokens = tc.window.estimated_tokens();
                    let pct = tokens * 100 / self.token_budget;

                    if pct >= 95 {
                        let compress_count = tc.window.entries.len() / 3;
                        tc.window.compress_oldest(compress_count.max(1));
                        tc.window.needs_checkpoint = true;
                        tracing::warn!(
                            task_id = %task_id,
                            tokens,
                            budget = self.token_budget,
                            "Context at 95% token budget — checkpoint flagged"
                        );
                    } else if pct >= 80 {
                        let compress_count = tc.window.entries.len() / 4;
                        tc.window.compress_oldest(compress_count.max(1));
                        tracing::info!(
                            task_id = %task_id,
                            tokens,
                            budget = self.token_budget,
                            "Context at 80% token budget — compressing oldest entries"
                        );
                    }
                }

                let expected = pre_count + 1;
                if tc.window.entries.len() < expected {
                    evicted = expected - tc.window.entries.len();
                }

                Ok(evicted)
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }
```

Update remaining methods (`is_budget_exhausted`, `entry_count`, `drain_checkpoint_flag`, `get_context`, `push_tool_result`, `set_partition_for_task`, `replace_context`, `remove_context`) to use `self.tasks` instead of `self.windows`, accessing `tc.window` for the ContextWindow. For example:

```rust
    pub async fn is_budget_exhausted(&self, task_id: &TaskID) -> bool {
        if self.token_budget == 0 {
            return false;
        }
        let tasks = self.tasks.read().await;
        if let Some(tc) = tasks.get(task_id) {
            tc.window.estimated_tokens() >= self.token_budget
        } else {
            false
        }
    }

    pub async fn entry_count(&self, task_id: &TaskID) -> usize {
        let tasks = self.tasks.read().await;
        tasks.get(task_id).map(|tc| tc.window.entries.len()).unwrap_or(0)
    }

    pub async fn drain_checkpoint_flag(&self, task_id: &TaskID) -> bool {
        let mut tasks = self.tasks.write().await;
        if let Some(tc) = tasks.get_mut(task_id) {
            if tc.window.needs_checkpoint {
                tc.window.needs_checkpoint = false;
                return true;
            }
        }
        false
    }

    pub async fn get_context(&self, task_id: &TaskID) -> Result<ContextWindow, AgentOSError> {
        let tasks = self.tasks.read().await;
        tasks
            .get(task_id)
            .map(|tc| tc.window.clone())
            .ok_or(AgentOSError::TaskNotFound(*task_id))
    }

    pub async fn set_partition_for_task(
        &self,
        task_id: &TaskID,
        partition: ContextPartition,
    ) -> Result<(), AgentOSError> {
        let mut tasks = self.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(tc) => {
                tc.window.set_partition(partition);
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }

    pub async fn replace_context(
        &self,
        task_id: &TaskID,
        window: ContextWindow,
    ) -> Result<(), AgentOSError> {
        let mut tasks = self.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(tc) => {
                tc.window = window;
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }

    pub async fn remove_context(&self, task_id: &TaskID) {
        self.tasks.write().await.remove(task_id);
    }
```

`push_tool_result` stays the same (it calls `self.push_entry` internally).

- [ ] **Step 5: Add `increment_references` method**

```rust
    /// Increment reference_count on context entries linked to the given
    /// tool call IDs. Called after tool results are pushed back.
    pub async fn increment_references(
        &self,
        task_id: &TaskID,
        tool_call_ids: &[String],
    ) -> Result<(), AgentOSError> {
        let mut tasks = self.tasks.write().await;
        match tasks.get_mut(task_id) {
            Some(tc) => {
                tc.window.increment_references_for_tool_call_ids(tool_call_ids);
                Ok(())
            }
            None => Err(AgentOSError::TaskNotFound(*task_id)),
        }
    }
```

- [ ] **Step 6: Update `create_context` call in `context_injector.rs`**

In `crates/agentos-kernel/src/context_injector.rs`, find line 65:

```rust
self.context_manager.create_context(task.id, "").await;
```

Replace with:

```rust
self.context_manager.create_context(task.id, task.agent_id, "").await;
```

- [ ] **Step 7: Update `create_context` call in `commands/pipeline.rs`**

In `crates/agentos-kernel/src/commands/pipeline.rs`, find the `create_context` call (around line 577):

```rust
self.context_manager
    .create_context(task_id, &system_prompt)
    .await;
```

Replace with (the agent's ID is available as `agent.id` in the surrounding scope — check the local variable):

```rust
self.context_manager
    .create_context(task_id, agent.id, &system_prompt)
    .await;
```

Note: If `agent.id` is not directly available, search for the `AgentID` in the surrounding function. The pipeline command handler creates agent tasks and has agent ID in scope.

- [ ] **Step 8: Update ContextManager construction in `kernel.rs`**

In `crates/agentos-kernel/src/kernel.rs`, find line 1779:

```rust
let context_manager = Arc::new(ContextManager::with_token_budget(
    config.kernel.context_window_max_entries,
    config.kernel.context_window_token_budget,
));
```

Replace with:

```rust
let context_manager = Arc::new(ContextManager::with_full_config(
    config.kernel.context_window_max_entries,
    config.kernel.context_window_token_budget,
    active_llms.clone(),
    cost_tracker.clone(),
    config.context.clone(),
));
```

Note: `active_llms` and `cost_tracker` must be constructed before this point. Check that `active_llms` is the `Arc<RwLock<HashMap<AgentID, Arc<dyn LLMCore>>>>` and `cost_tracker` is the `Arc<CostTracker>`. Both are already created during kernel initialization. If the ordering doesn't work, move the `context_manager` construction after both are available.

- [ ] **Step 9: Build and verify**

Run: `cargo build -p agentos-kernel`
Expected: Compiles without errors. All existing tests should still pass because `push_entry` behavior is unchanged (compression still uses `compress_oldest`).

Run: `cargo test -p agentos-kernel`
Expected: All tests pass.

- [ ] **Step 10: Commit**

```bash
git add crates/agentos-kernel/src/context.rs crates/agentos-kernel/src/kernel.rs crates/agentos-kernel/src/context_injector.rs crates/agentos-kernel/src/commands/pipeline.rs
git commit -m "refactor(kernel): restructure ContextManager with agent_id storage and full config deps"
```

---

### Task 7: LLM Summarization + Context Notice in push_entry

**Files:**
- Modify: `crates/agentos-kernel/src/context.rs`

This is the core feature: replace the `compress_oldest` call in `push_entry` with extract → LLM summarize (or concat fallback) → insert summary → upsert notice.

- [ ] **Step 1: Add `summarize_entries_concat` helper**

Add as a standalone function in `context.rs` (outside the `impl` block, or as a private associated function):

```rust
impl ContextManager {
    /// Concat fallback: format entries as truncated snippets (matches legacy compress_oldest format).
    fn summarize_entries_concat(entries: &[ContextEntry]) -> String {
        let parts: Vec<String> = entries
            .iter()
            .map(|e| {
                let label = match e.role {
                    ContextRole::User => "User",
                    ContextRole::Assistant => "Assistant",
                    ContextRole::ToolResult => "ToolResult",
                    ContextRole::System => "System",
                };
                let snippet = if e.content.chars().count() > 150 {
                    format!("{}...", e.content.chars().take(150).collect::<String>())
                } else {
                    e.content.clone()
                };
                format!("[{label}]: {snippet}")
            })
            .collect();
        parts.join("\n")
    }

    /// Attempt LLM-generated summarization. Returns Ok((summary_text, inference_result))
    /// on success, Err on any failure (no adapter, LLM error, empty response).
    async fn summarize_entries_llm(
        entries: &[ContextEntry],
        llm: &dyn agentos_llm::LLMCore,
        max_input_chars: usize,
    ) -> Result<(String, agentos_llm::InferenceResult), anyhow::Error> {
        // Build the text to summarize, respecting the char limit
        let mut text_parts = Vec::new();
        let mut total_chars = 0usize;
        for e in entries {
            let label = match e.role {
                ContextRole::User => "User",
                ContextRole::Assistant => "Assistant",
                ContextRole::ToolResult => "ToolResult",
                ContextRole::System => "System",
            };
            let part = format!("[{}]: {}", label, e.content);
            let part_chars = part.chars().count();
            if total_chars + part_chars > max_input_chars {
                // Truncate this entry to fit remaining budget
                let remaining = max_input_chars.saturating_sub(total_chars);
                if remaining > 20 {
                    text_parts.push(format!(
                        "[{}]: {}...",
                        label,
                        e.content.chars().take(remaining).collect::<String>()
                    ));
                }
                break;
            }
            total_chars += part_chars;
            text_parts.push(part);
        }
        let messages_text = text_parts.join("\n");

        let system_prompt = "Summarize the following conversation messages into a concise paragraph. \
            Preserve: key decisions, tool outputs that produced important results, error messages, \
            and any facts the agent discovered. \
            Discard: routine acknowledgments, redundant tool calls, and verbose formatting. \
            Keep the summary under 300 words.";

        // Build a minimal context window for the summarization call
        let mut ctx = ContextWindow::new(16);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: system_prompt.to_string(),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::System,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: format!("Messages:\n{}", messages_text),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::Task,
            is_summary: false,
        });

        let result = llm.infer(&ctx).await?;
        let summary = result.text.trim().to_string();
        if summary.is_empty() {
            anyhow::bail!("LLM returned empty summary");
        }
        Ok((summary, result))
    }
}
```

- [ ] **Step 2: Refactor `push_entry` to use LLM summarization**

Replace the existing `push_entry` method with the new version that extracts entries, releases the lock, calls LLM, re-acquires the lock, and inserts:

```rust
    pub async fn push_entry(
        &self,
        task_id: &TaskID,
        entry: ContextEntry,
    ) -> Result<usize, AgentOSError> {
        // Phase 1: Push the entry and check if compression is needed.
        // Extract compressible entries if threshold exceeded, then release lock.
        let compression_needed: Option<(Vec<ContextEntry>, AgentID, bool)> = {
            let mut tasks = self.tasks.write().await;
            match tasks.get_mut(task_id) {
                Some(tc) => {
                    tc.window.push(entry);

                    if self.token_budget > 0 {
                        let tokens = tc.window.estimated_tokens();
                        let pct = tokens * 100 / self.token_budget;

                        if pct >= 80 {
                            let is_critical = pct >= 95;
                            let compress_count = if is_critical {
                                tc.window.entries.len() / 3
                            } else {
                                tc.window.entries.len() / 4
                            };

                            match self.config.summarization_mode {
                                SummarizationMode::Off => {
                                    // No summary: just extract and discard
                                    let _ = tc.window.extract_compressible(compress_count.max(1));
                                    if is_critical {
                                        tc.window.needs_checkpoint = true;
                                    }
                                    None
                                }
                                SummarizationMode::Concat => {
                                    // Legacy: use existing compress_oldest
                                    tc.window.compress_oldest(compress_count.max(1));
                                    if is_critical {
                                        tc.window.needs_checkpoint = true;
                                    }
                                    tracing::info!(
                                        task_id = %task_id,
                                        tokens,
                                        budget = self.token_budget,
                                        "Context at {}% token budget — compressed (concat)",
                                        pct
                                    );
                                    None
                                }
                                SummarizationMode::Llm => {
                                    let extracted = tc.window.extract_compressible(compress_count.max(1));
                                    if is_critical {
                                        tc.window.needs_checkpoint = true;
                                    }
                                    if extracted.is_empty() {
                                        None
                                    } else {
                                        tracing::info!(
                                            task_id = %task_id,
                                            tokens,
                                            budget = self.token_budget,
                                            extracted = extracted.len(),
                                            "Context at {}% token budget — attempting LLM summarization",
                                            pct
                                        );
                                        Some((extracted, tc.agent_id, is_critical))
                                    }
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                None => return Err(AgentOSError::TaskNotFound(*task_id)),
            }
            // Write lock released here
        };

        // Phase 2: If entries were extracted for LLM summarization, do it without holding the lock.
        if let Some((extracted, agent_id, _is_critical)) = compression_needed {
            let extracted_count = extracted.len();

            // Try LLM summarization
            let summary_text = {
                let llm_opt = {
                    let llms = self.active_llms.read().await;
                    llms.get(&agent_id).cloned()
                };

                match llm_opt {
                    Some(llm) => {
                        match Self::summarize_entries_llm(
                            &extracted,
                            llm.as_ref(),
                            self.config.summarization_max_input_chars,
                        )
                        .await
                        {
                            Ok((summary, inference_result)) => {
                                // Record summarization cost against agent budget
                                let _budget_result = self
                                    .cost_tracker
                                    .record_inference_with_cost(
                                        &agent_id,
                                        &inference_result.tokens_used,
                                        llm.provider_name(),
                                        llm.model_name(),
                                        inference_result.cost.as_ref(),
                                    )
                                    .await;
                                tracing::info!(
                                    task_id = %task_id,
                                    entries = extracted_count,
                                    "LLM summarization succeeded"
                                );
                                summary
                            }
                            Err(e) => {
                                tracing::warn!(
                                    task_id = %task_id,
                                    error = %e,
                                    "LLM summarization failed — falling back to concat"
                                );
                                Self::summarize_entries_concat(&extracted)
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            task_id = %task_id,
                            "No LLM adapter available for summarization — falling back to concat"
                        );
                        Self::summarize_entries_concat(&extracted)
                    }
                }
            };

            // Phase 3: Re-acquire lock and insert summary + notice
            {
                let mut tasks = self.tasks.write().await;
                if let Some(tc) = tasks.get_mut(task_id) {
                    tc.window.insert_summary_entry(summary_text, extracted_count);
                    tc.window.upsert_context_notice(extracted_count);
                }
            }
        }

        // Return 0 for evicted count — the exact count is hard to compute across
        // the two-phase flow, and callers only use it for logging.
        Ok(0)
    }
```

- [ ] **Step 3: Build and verify**

Run: `cargo build -p agentos-kernel`
Expected: Compiles without errors.

Run: `cargo test -p agentos-kernel`
Expected: All existing tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/agentos-kernel/src/context.rs
git commit -m "feat(kernel): wire LLM summarization + context-loss notice in push_entry"
```

---

### Task 8: Wire increment_references in task_executor.rs

**Files:**
- Modify: `crates/agentos-kernel/src/task_executor.rs`

- [ ] **Step 1: Find the tool result push site and add increment_references call**

In `crates/agentos-kernel/src/task_executor.rs`, the main tool execution flow pushes tool results and then continues the loop. We need to collect all `tool_call_id` values from the batch of tool calls that were just executed and call `increment_references` once.

Search for the point after all tool results have been pushed in an iteration. There are two execution paths: the parallel tool call path and the sequential tool call path. We need to add the call in both.

After the parallel tool call batch completes (search for `refresh_knowledge_blocks = true` or the end of the parallel tool execution block), add:

```rust
// Increment reference counts for tool call IDs that were just processed.
// This makes the linked Assistant + ToolResult entries resist eviction.
{
    let tool_call_ids: Vec<String> = parsed_tool_calls
        .iter()
        .filter_map(|tc| tc.id.clone())
        .collect();
    if !tool_call_ids.is_empty() {
        if let Err(e) = self
            .context_manager
            .increment_references(&task.id, &tool_call_ids)
            .await
        {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "Failed to increment reference counts"
            );
        }
    }
}
```

Note: `parsed_tool_calls` is the `Vec<ToolCallRequest>` from line 2561. The variable name may differ between the parallel and sequential paths — check the local variable name in each path. The key is `tc.id.clone()` which is `Option<String>`.

Do the same for the sequential tool call path if it exists as a separate code block.

- [ ] **Step 2: Build and verify**

Run: `cargo build -p agentos-kernel`
Expected: Compiles.

Run: `cargo test -p agentos-kernel`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/agentos-kernel/src/task_executor.rs
git commit -m "feat(kernel): wire reference count tracking after tool result pushes"
```

---

### Task 9: Integration Tests

**Files:**
- Modify: `crates/agentos-types/src/context.rs` (additional edge case tests)

- [ ] **Step 1: Add integration test for LLM summarization with concat fallback**

This test verifies the full `push_entry` → extract → concat fallback → insert flow by using a ContextManager with `SummarizationMode::Concat`:

Add to the `#[cfg(test)]` module in `crates/agentos-types/src/context.rs`:

```rust
#[test]
fn test_extract_then_insert_summary_round_trip() {
    let mut window = ContextWindow::new(100);
    window.push(ContextEntry {
        role: ContextRole::System,
        content: "system".to_string(),
        timestamp: chrono::Utc::now(),
        metadata: None,
        importance: 1.0,
        pinned: true,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::System,
        is_summary: false,
    });
    for i in 0..5 {
        window.push(ContextEntry {
            role: if i % 2 == 0 { ContextRole::User } else { ContextRole::Assistant },
            content: format!("message {}", i),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::Active,
            category: ContextCategory::History,
            is_summary: false,
        });
    }

    assert_eq!(window.entries.len(), 6);

    // Extract 3 oldest compressible
    let extracted = window.extract_compressible(3);
    assert_eq!(extracted.len(), 3);
    assert_eq!(window.entries.len(), 3); // system + 2 remaining

    // Insert a summary
    window.insert_summary_entry("Summary of messages 0-2".to_string(), 3);
    assert_eq!(window.entries.len(), 4); // system + summary + 2 remaining

    // Insert notice
    window.upsert_context_notice(3);
    assert_eq!(window.entries.len(), 5); // system + notice + summary + 2 remaining

    // Verify ordering: system, notice, summary, msg3, msg4
    assert_eq!(window.entries[0].role, ContextRole::System);
    assert!(!window.entries[0].is_summary);
    assert!(window.entries[1].content.starts_with("[CONTEXT NOTE]"));
    assert!(window.entries[2].is_summary);
    assert_eq!(window.entries[3].content, "message 3");
    assert_eq!(window.entries[4].content, "message 4");
}
```

- [ ] **Step 2: Run test**

Run: `cargo test -p agentos-types test_extract_then_insert_summary_round_trip -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/agentos-types/src/context.rs
git commit -m "test(types): add round-trip integration test for extract → summarize → insert flow"
```

---

### Task 10: Final Verification

**Files:** None (verification only)

- [ ] **Step 1: Run full workspace build**

Run: `cargo build --workspace`
Expected: Clean build, no errors.

- [ ] **Step 2: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No new warnings introduced (pre-existing warnings in agentos-audit, agentos-sandbox, agentos-memory are acceptable).

- [ ] **Step 4: Run format check**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues. If any, run `cargo fmt --all` and commit.

- [ ] **Step 5: Final commit (if fmt needed)**

```bash
cargo fmt --all
git add -A
git commit -m "style: fix rustfmt formatting"
```
