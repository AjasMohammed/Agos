---
title: Memory System
tags:
  - docs
  - memory
  - reference
  - handbook
  - v3
date: 2026-03-17
status: complete
effort: 3h
priority: high
---

# Memory System

> How AgentOS agents remember facts, recall history, and learn procedures across tasks.

---

## Memory Architecture Overview

AgentOS uses a **four-tier memory architecture** plus a set of supporting systems that manage how information flows in and out of each tier.

```
┌─────────────────────────────────────────────────────────────────┐
│                     TASK EXECUTION                              │
│                                                                 │
│  ┌────────────────────────────────────────────────────────┐     │
│  │  Tier 1: Working Memory (Context Window)               │     │
│  │  In-memory · Per-task · Token-budget managed           │     │
│  └──────────────┬──────────────────────┬──────────────────┘     │
│                 │ auto-inject at start │ auto-write on complete │
│                 ▼                      ▼                        │
│  ┌──────────────────────┐  ┌──────────────────────────────┐     │
│  │ Tier 2: Episodic     │  │ Memory Blocks                │     │
│  │ Per-task event log   │  │ Named agent working state    │     │
│  │ SQLite · FTS5        │  │ SQLite · label-keyed         │     │
│  └──────────┬───────────┘  └──────────────────────────────┘     │
│             │ consolidation (background)                        │
│             ▼                                                   │
│  ┌──────────────────────┐  ┌──────────────────────────────┐     │
│  │ Tier 3: Semantic     │  │ Tier 4: Procedural           │     │
│  │ Cross-task knowledge │  │ Learned step-by-step skills  │     │
│  │ FTS5 + vector (RRF)  │  │ FTS5 + vector (RRF)          │     │
│  └──────────────────────┘  └──────────────────────────────┘     │
└─────────────────────────────────────────────────────────────────┘
         ▲                          │
         │  Retrieval Gate          │ Memory Extraction
         │  (classifies + queries)  │ (auto-facts from results)
         └──────────────────────────┘
```

### When each tier is used

| Phase | Action |
|-------|--------|
| Task starts | Retrieval Gate classifies the task prompt; relevant episodic, semantic, and procedural entries are fetched and injected into the context window |
| During execution | Tool results feed the Memory Extraction engine, which auto-writes new facts into Semantic memory |
| Task completes | Episode events are persisted to Episodic memory |
| Background | Consolidation engine scans Episodic memory for repeated patterns and distills them into Procedural memory |

---

## Tier 1 — Working Memory (Context Window)

Working memory is the active in-memory buffer that the LLM reads on every inference. It is **per-task**, **token-budget-managed**, and discarded when the task ends.

### How the context window is built

The **ContextCompiler** assembles the window from structured inputs in a fixed priority order:

```
System Prompt → Tool Descriptions → Knowledge Blocks → Conversation History → Task Prompt
```

This ordering follows the **primacy/recency principle**: the most important framing (system prompt) appears first; the most immediately relevant context (task prompt) appears last.

### Entry categories

| Category | Contents | Evictable? |
|----------|----------|------------|
| `System` | System prompt, agent persona, safety instructions | No — pinned at importance 1.0 |
| `Tools` | Tool descriptions injected by the compiler | Yes |
| `Knowledge` | Retrieved memory blocks from Retrieval Gate | Yes |
| `History` | Conversation turns, tool results | Yes (oldest first) |
| `Task` | Current task prompt | Yes |

### Token budget enforcement

When the context window fills up, the kernel automatically compresses it:

| Threshold | Action |
|-----------|--------|
| 80% full | Evict the oldest 25% of evictable entries |
| 95% full | Evict the oldest 33% of evictable entries + set a checkpoint flag |

The system prompt is **never evicted** (pinned entry). History entries are selected newest-first; pinned entries (if any) are always preserved.

### Context budget configuration

The `[context_budget]` section in `config/default.toml` controls how total tokens are divided among entry categories:

| Key | Default | Description |
|-----|---------|-------------|
| `total_tokens` | `128000` | Total token budget for the context window |
| `reserve_pct` | `0.25` | Fraction reserved for LLM output (not consumed by inputs) |
| `system_pct` | `0.15` | Fraction of *usable* tokens allocated to system prompt + tools |
| `tools_pct` | `0.18` | Fraction allocated to tool descriptions |
| `knowledge_pct` | `0.30` | Fraction allocated to retrieved memory blocks |
| `history_pct` | `0.25` | Fraction allocated to conversation history |
| `task_pct` | `0.12` | Fraction allocated to the current task prompt |

> [!tip]
> `reserve_pct` is subtracted first. The remaining percentages are applied to the usable token count, not the total. Percentages need not sum to 1.0 — the compiler fits each category independently and may leave slack.

---

## Tier 2 — Episodic Memory

Episodic memory is an **append-only SQLite event log** that records what happened during each task. It persists across restarts and can be recalled in future tasks.

### Episode types

Each event written to episodic memory has one of the following types:

| Type | When emitted |
|------|-------------|
| `Intent` | An intent message is received by the kernel |
| `ToolCall` | An agent calls a tool |
| `ToolResult` | A tool returns a result |
| `LLMResponse` | The LLM produces an inference output |
| `AgentMessage` | One agent sends a message to another |
| `UserPrompt` | A user-supplied prompt is received |
| `SystemEvent` | A system-level event (task completion, errors, milestone outcomes) |

### What gets stored per episode

```
task_id     — which task this event belongs to
agent_id    — which agent produced the event
entry_type  — one of the types above
content     — full event content
summary     — short searchable summary (for FTS5)
metadata    — JSON blob for structured extras (e.g., outcome="success")
timestamp   — UTC timestamp
trace_id    — optional distributed trace correlation ID
```

### Recall and access

Agents recall episodic memory using the `memory-search` tool with `scope: "episodic"`. Retrieval is permission-gated:

- **Own task episodes** — allowed by default
- **Other tasks (same agent)** — requires `memory.episodic:r`
- **Global (all agents, all tasks)** — requires the global episodic read permission

The store supports full-text search (FTS5) across summaries and content, filtered by `task_id`, `agent_id`, or time range.

### Auto-injection at task start

When a new task starts, the Retrieval Gate evaluates the task prompt. If it matches episodic signal words (e.g., "last time", "previously", "remember"), the most relevant past episodes are fetched and injected into the context window as `Knowledge` entries.

---

## Tier 3 — Semantic Memory

Semantic memory is a **global, cross-task, cross-agent knowledge store** backed by SQLite with FTS5 full-text search and fastembed vector embeddings.

### How search works

Semantic search uses **Reciprocal Rank Fusion (RRF)** to merge two independent rankings:

1. **FTS5 phase** — keyword/text relevance using SQLite's built-in full-text search
2. **Vector phase** — semantic similarity via cosine distance on 384-dimensional `AllMiniLML6V2` embeddings

The RRF score combines both ranks, making results that appear high in *both* lists float to the top. This handles both exact keyword matches and paraphrased queries.

### Text chunking

Large content is automatically split into overlapping chunks before embedding:
- **Chunk size:** up to 2000 characters
- **Overlap:** 200 characters between consecutive chunks
- Chunks are whitespace-aware and UTF-8 safe

### Permissions

| Operation | Required permission |
|-----------|-------------------|
| Read / search | `memory.semantic:r` |
| Write / update | `memory.semantic:w` |

### Tools

| Tool | What it does |
|------|-------------|
| `memory-search` | Hybrid FTS5+vector search; `scope: "semantic"` (default) |
| `memory-write` | Write a fact to semantic memory with optional key and tags |
| `archival-insert` | Insert a note into long-term archival (alias for semantic write) |
| `archival-search` | Search archival semantic memory; returns key, content, and score |

---

## Tier 4 — Procedural Memory

Procedural memory stores **named, step-by-step skill patterns** that agents have learned from repeated successful task executions.

### What a procedure looks like

```
name          — human-readable procedure name
description   — what this procedure accomplishes
pre_conditions — conditions that must be true before running
post_conditions — expected state after completion
steps         — ordered list of ProcedureSteps
  └─ order          — step sequence number
  └─ action         — natural language description of the action
  └─ tool           — optional: which tool to invoke
  └─ expected_outcome — optional: what a successful result looks like
success_count — how many times this procedure succeeded
failure_count — how many times it failed
source_episodes — list of episode IDs that informed this procedure
tags          — searchable labels
```

### How procedures are created

Procedures are not written manually — they are **distilled automatically** by the Consolidation engine (see [[#Memory Consolidation]]) from clusters of successful episodic events.

Agents can also retrieve procedures on-demand. When a task prompt contains procedural signal words ("how to", "steps", "guide"), the Retrieval Gate queries the procedural store and injects matching procedures into the context window.

### Search

Procedural search uses the same hybrid FTS5+vector RRF approach as Semantic memory.

---

## Memory Blocks

Memory blocks are **short-form, labeled persistent state** for individual agents. Think of them as an agent's working notebook — small named slots it can read and update between tasks.

### Characteristics

| Property | Value |
|----------|-------|
| Scope | Per-agent |
| Label constraint | Unique per agent; 1–128 characters |
| Content limit | 2048 characters |
| Storage | SQLite (`memory_blocks.db`) |
| Persistence | Survives task boundaries and restarts |

### Use cases

- Storing user preferences discovered during past tasks
- Tracking working assumptions between task runs
- Maintaining task-specific counters or state flags

### Tools

| Tool | Permission | What it does |
|------|-----------|-------------|
| `memory-block-read` | `memory.blocks:r` | Read a block by label |
| `memory-block-write` | `memory.blocks:w` | Write or update a block (upsert) |
| `memory-block-list` | `memory.blocks:r` | List all blocks for this agent |
| `memory-block-delete` | `memory.blocks:w` | Delete a block by label |

### Example

```json
// memory-block-write
{
  "label": "user-preferred-language",
  "content": "TypeScript"
}

// memory-block-read
{
  "label": "user-preferred-language"
}
// → { "label": "user-preferred-language", "content": "TypeScript" }
```

---

## Memory Extraction

The Memory Extraction engine **automatically extracts facts from tool results** and writes them to Semantic memory without the agent needing to do anything.

### How it works

After each tool returns a result, the extraction engine:

1. Checks whether the result meets the minimum length threshold (default: 50 characters)
2. Applies a **tool-specific extractor** to identify candidate facts:
   - `http-client` results → URL, status, extracted data fields
   - `shell-exec` results → command output fragments
   - `file-reader` results → file key-value facts
   - `data-parser` results → structured data elements
3. For each candidate fact, performs a **semantic similarity check** against existing memory entries
4. Decides what to do based on the similarity score:

| Score | Action |
|-------|--------|
| > 0.95 | Skip — this is a duplicate |
| 0.85 – 0.95 | Update the existing entry — this is related information |
| < 0.85 | Add a new entry — this is novel |

### Configuration

```toml
[memory.extraction]
enabled = true
conflict_threshold = 0.85   # similarity threshold between "update" and "add new"
max_facts_per_result = 5    # maximum facts extracted from a single tool result
min_result_length = 50      # minimum result length (chars) to attempt extraction
```

### Extraction report

Each extraction run returns counts of:
- `added` — new facts written
- `updated` — existing facts revised
- `skipped` — duplicates discarded

---

## Memory Consolidation

The Consolidation engine runs **periodically in the background**, scanning Episodic memory for repeated patterns and distilling them into Procedural memory.

### How consolidation works

1. Query Episodic memory for `SystemEvent` entries with `outcome = "success"`
2. Cluster episodes by the **first 4 tokens** of their summary (case-insensitive)
3. For each cluster with at least `min_pattern_occurrences` episodes:
   - Extract the tools called across those episodes
   - Generate a `Procedure` with steps derived from the tool sequence
   - Check whether a similar procedure already exists (RRF score > 0.90 → skip)
   - Write the new procedure to Procedural memory

### Configuration

```toml
[memory.consolidation]
enabled = true
min_pattern_occurrences = 3    # minimum episodes in a cluster to form a procedure
task_completions_trigger = 100 # run consolidation after every N task completions
time_trigger_hours = 24        # also run after N hours regardless of task count
max_episodes_per_cycle = 500   # maximum episodes to scan per consolidation run
```

### Consolidation report

Each consolidation cycle reports:
- `patterns_found` — clusters meeting the occurrence threshold
- `created` — new procedures written
- `skipped_existing` — patterns that matched existing procedures
- `failed` — episodes that could not be processed

---

## Retrieval Gate

The Retrieval Gate determines **which memory indices to search** for a given query, avoiding unnecessary lookups.

### Query classification

The gate inspects signal words in the query text:

| Signal words | Index queried |
|-------------|--------------|
| "remember", "last time", "previously", "earlier", "before" | Episodic |
| "how to", "steps", "procedure", "guide", "tutorial" | Procedural |
| "find tool", "what tool", "tool for" | Tools |
| "what is", "explain", "define", "tell me about" | Semantic |
| Short/trivial queries ("ok", "thanks", < threshold) | None — skipped |

A single query can match multiple indices; searches run in parallel via the **RetrievalExecutor**.

### Result formatting

Retrieved entries are wrapped in typed XML blocks before injection into the context window:

```
[RETRIEVED_SEMANTIC]
key: deployment-checklist
...content...
[/RETRIEVED_SEMANTIC]

[RETRIEVED_EPISODIC]
...event summary...
[/RETRIEVED_EPISODIC]
```

Duplicate results (detected by content hash) are removed before injection.

---

## Context Compilation

Before each LLM inference, the **ContextCompiler** assembles the final context window from structured inputs.

### Inputs

| Input field | Source |
|-------------|--------|
| `system_prompt` | Agent definition or kernel default |
| `tool_descriptions` | Registered tools for this agent |
| `agent_directory` | Other agents visible to this agent |
| `knowledge_blocks` | Retrieved memory from Retrieval Gate |
| `history` | Conversation turns and tool results |
| `task_prompt` | Current task description |

### Ordering

The compiler writes entries in a fixed order that maximizes LLM comprehension:

```
1. System prompt       ← framing and identity (primacy)
2. Tool descriptions   ← available capabilities
3. Knowledge blocks    ← retrieved memories and facts
4. Conversation history ← recent context
5. Task prompt         ← immediate intent (recency)
```

### Budget enforcement

Each category has its own token allocation (see [[#Context budget configuration]]). If a category's content exceeds its budget:
- **History**: newest entries are kept, oldest are dropped
- **Knowledge blocks**: entries are truncated to fit
- **Other categories**: truncated to the per-category limit

Pinned entries (importance = 1.0) bypass eviction and are always included.

---

## Additional Memory Tools

Beyond the tier-specific tools documented above, the memory system provides the following tools:

### Procedural Memory Tools

| Tool | Permission | Description |
|------|-----------|-------------|
| `procedure-search` | `memory.procedural:r` | Search procedures by query |
| `procedure-create` | `memory.procedural:w` | Create a new procedure |
| `procedure-list` | `memory.procedural:r` | List all procedures |
| `procedure-delete` | `memory.procedural:w` | Delete a procedure |

### Episodic Memory Tools

| Tool | Permission | Description |
|------|-----------|-------------|
| `episodic-list` | `memory.episodic:r` | List recent episodes |

### General Memory Tools

| Tool | Permission | Description |
|------|-----------|-------------|
| `memory-read` | `memory:r` | Read a specific memory entry |
| `memory-delete` | `memory:w` | Delete a memory entry |
| `memory-stats` | `memory:r` | Show memory system statistics |

### Context Memory Tools

| Tool | Permission | Description |
|------|-----------|-------------|
| `context-memory-read` | `agent.context:r` | Read agent's context memory document |
| `context-memory-update` | `agent.context:w` | Update agent's context memory document |

---

## Full Configuration Reference

### `[memory]`

| Key | Default | Description |
|-----|---------|-------------|
| `model_cache_dir` | `"models"` | Directory for fastembed model cache (~23 MB, downloaded once) |

### `[memory.extraction]`

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `true` | Enable automatic fact extraction from tool results |
| `conflict_threshold` | `0.85` | Similarity score boundary between "update existing" and "add new" |
| `max_facts_per_result` | `5` | Maximum facts extracted from a single tool result |
| `min_result_length` | `50` | Minimum result length (characters) before extraction is attempted |

### `[memory.consolidation]`

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `true` | Enable background consolidation of episodic patterns into procedures |
| `min_pattern_occurrences` | `3` | Minimum episode cluster size to create a procedure |
| `task_completions_trigger` | `100` | Run consolidation after every N task completions |
| `time_trigger_hours` | `24` | Run consolidation every N hours regardless of task count |
| `max_episodes_per_cycle` | `500` | Maximum episodes scanned per consolidation cycle |

### `[context_budget]`

| Key | Default | Description |
|-----|---------|-------------|
| `total_tokens` | `128000` | Total token capacity of the context window |
| `reserve_pct` | `0.25` | Fraction reserved for LLM output tokens |
| `system_pct` | `0.15` | Fraction of usable tokens for the system prompt |
| `tools_pct` | `0.18` | Fraction for tool descriptions |
| `knowledge_pct` | `0.30` | Fraction for retrieved memory blocks |
| `history_pct` | `0.25` | Fraction for conversation history |
| `task_pct` | `0.12` | Fraction for the current task prompt |

> [!note]
> Percentages apply to *usable* tokens (`total_tokens × (1 - reserve_pct)`), not the raw total. They are enforced independently per category — the compiler does not require them to sum to 1.0.

---

## Related

- [[Memory Context Architecture Plan]] — design decisions and rationale for the memory system
- [[Memory Context Data Flow]] — data flow diagrams
- [[Security Model]] — capability token requirements for memory permissions
- [[Tool System]] — how tools invoke memory operations
