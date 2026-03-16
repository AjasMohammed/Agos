---
title: Handbook Memory System
tags:
  - docs
  - memory
  - v3
  - plan
date: 2026-03-13
status: planned
effort: 3h
priority: high
---

# Handbook Memory System

> Write the Memory System chapter covering all three memory tiers (working, episodic, semantic), the procedural store, memory extraction, consolidation, retrieval gate, context compilation, and memory block tools.

---

## Why This Subtask
The memory system is one of the most architecturally complex parts of AgentOS, spanning working memory (context windows), episodic memory (per-task history), semantic memory (cross-task knowledge), and procedural memory (learned procedures). Users need to understand how agents remember, recall, and learn across tasks. The existing `obsidian-vault/reference/Memory System.md` is an internal architecture reference, not a user-facing guide.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Memory tiers | 3 tiers listed in architecture doc | Full explanation of all tiers with user-facing examples |
| Procedural memory | Internal only | User documentation: what procedures are, how they are learned |
| Memory extraction | Internal only | User documentation: automatic fact extraction from task results |
| Consolidation | Internal only | User documentation: periodic pattern detection across episodes |
| Retrieval gate | Internal only | User documentation: freshness-based retrieval optimization |
| Context compilation | Internal only | User documentation: how the kernel assembles context windows |
| Context budget | Config keys exist | Full documentation of `[context_budget]` config section |
| Memory tools | Tools exist but not documented together | Comprehensive list of all memory-related tools |

---

## What to Do

### Write `10-Memory System.md`

Read these source files for ground truth:
- `crates/agentos-memory/src/lib.rs` -- module exports
- `crates/agentos-memory/src/semantic.rs` -- `SemanticStore`: keyword search, embedding storage
- `crates/agentos-memory/src/episodic.rs` -- `EpisodicStore`: per-task episode recording and recall
- `crates/agentos-memory/src/procedural.rs` -- `ProceduralStore`: learned procedure patterns
- `crates/agentos-memory/src/embedder.rs` -- `Embedder`: text embedding generation
- `crates/agentos-memory/src/types.rs` -- `MemoryEntry`, `MemoryChunk`, `EpisodicEntry`, `Procedure`, `RecallQuery`, `RecallResult`
- `crates/agentos-kernel/src/context_compiler.rs` -- context window assembly logic
- `crates/agentos-kernel/src/context.rs` -- `ContextWindow`, `ContextEntry`, `SemanticEviction`
- `crates/agentos-kernel/src/retrieval_gate.rs` -- freshness-based retrieval optimization
- `crates/agentos-kernel/src/memory_extraction.rs` -- automatic fact extraction from tool results
- `crates/agentos-kernel/src/consolidation.rs` -- periodic pattern consolidation
- `crates/agentos-kernel/src/memory_blocks.rs` -- memory block CRUD operations
- `crates/agentos-tools/src/memory_search.rs` -- memory-search tool
- `crates/agentos-tools/src/memory_write.rs` -- memory-write tool
- `crates/agentos-tools/src/memory_block_read.rs` -- memory-block-read tool
- `crates/agentos-tools/src/memory_block_write.rs` -- memory-block-write tool
- `crates/agentos-tools/src/memory_block_list.rs` -- memory-block-list tool
- `crates/agentos-tools/src/memory_block_delete.rs` -- memory-block-delete tool
- `crates/agentos-tools/src/archival_insert.rs` -- archival-insert tool
- `crates/agentos-tools/src/archival_search.rs` -- archival-search tool
- `config/default.toml` -- `[memory]`, `[memory.extraction]`, `[memory.consolidation]`, `[context_budget]` sections

The chapter must include:

**Section: Memory Architecture Overview**
- Diagram showing all 4 tiers and how they interact
- When each tier is used during task execution

**Section: Tier 1 -- Working Memory (Context Window)**
- Per-task, in-memory ring buffer of `ContextEntry` items
- Entries: system prompt, agent directory, conversation history, tool results
- Token budget management via `[context_budget]` config
- Semantic eviction: when window overflows, oldest entries are evicted based on importance score
- Entry categories: System, Knowledge, History, ToolResult, Task
- Pinned entries (never evicted)
- Partitioning per task

**Section: Tier 2 -- Episodic Memory**
- Per-task history persisted after task completion
- Episode types: `TaskExecution`, `ToolCall`, `Error`, `Observation`
- Auto-inject at task start: relevant past episodes are recalled
- `EpisodicStore` API: `record()`, `recall()`
- Auto-write on task completion (if enabled)

**Section: Tier 3 -- Semantic Memory**
- Global, cross-task, cross-agent knowledge store
- Keyword-based search (vector search planned)
- Permission-gated: requires `memory.semantic:r` to read, `memory.semantic:w` to write
- Tools: `memory-search`, `memory-write`

**Section: Procedural Memory**
- Learned procedures (step-by-step patterns extracted from repeated task executions)
- `Procedure` type: name, steps, confidence score
- Used for context enrichment: relevant procedures injected into context

**Section: Memory Extraction**
- Automatic fact extraction from tool results and LLM responses
- Config: `[memory.extraction]` -- `enabled`, `conflict_threshold`, `max_facts_per_result`, `min_result_length`
- How conflicts are detected and resolved

**Section: Memory Consolidation**
- Periodic pattern detection across episodic entries
- Config: `[memory.consolidation]` -- `enabled`, `min_pattern_occurrences`, `task_completions_trigger`, `time_trigger_hours`, `max_episodes_per_cycle`
- When consolidation runs: after N task completions or N hours

**Section: Retrieval Gate**
- Freshness-based retrieval optimization
- Caches retrieval results and reuses them when context has not changed significantly
- Refresh vs reuse decision metrics

**Section: Context Compilation**
- How the kernel assembles the context window before each LLM inference
- Budget allocation: `total_tokens`, `reserve_pct`, `system_pct`, `tools_pct`, `knowledge_pct`, `history_pct`, `task_pct`
- Priority ordering of entries

**Section: Memory Block Tools**
- `memory-block-read`, `memory-block-write`, `memory-block-list`, `memory-block-delete`
- Named persistent memory blocks for structured agent state
- `archival-insert`, `archival-search` for long-term archival

**Section: Configuration Reference**
- Full `[memory]`, `[memory.extraction]`, `[memory.consolidation]`, `[context_budget]` config tables

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/10-Memory System.md` | Create new |

---

## Prerequisites
[[01-foundation-chapters]] must be complete (architecture context needed).

---

## Test Plan
- File exists
- All 4 memory tiers documented (working, episodic, semantic, procedural)
- All memory-related config keys from `config/default.toml` are documented
- Memory extraction and consolidation sections have config tables
- Context compilation budget allocation is documented
- All memory block tools are listed

---

## Verification
```bash
test -f obsidian-vault/reference/handbook/10-Memory\ System.md

# All tiers covered
grep -c "Working Memory\|Episodic Memory\|Semantic Memory\|Procedural Memory" \
  obsidian-vault/reference/handbook/10-Memory\ System.md
# Should be >= 4

# Config sections covered
grep -c "memory.extraction\|memory.consolidation\|context_budget" \
  obsidian-vault/reference/handbook/10-Memory\ System.md
# Should be >= 3
```
