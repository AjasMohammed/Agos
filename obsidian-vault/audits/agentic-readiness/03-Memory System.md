---
title: "Audit #3: Memory System"
tags:
  - audit
  - memory
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 2h
priority: critical
---

# Audit #3: Memory System

> Evaluating semantic, episodic, and procedural memory — the three tiers that give me persistent knowledge across tasks and sessions.

---

## Scope

- `crates/agentos-memory/src/` — semantic.rs, episodic.rs, procedural.rs, embedder.rs, types.rs
- Related tools: memory-write, memory-search, memory-read, memory-delete, memory-stats, archival-insert, archival-search, episodic-list, procedure-create/delete/list/search, memory-block-*

As an AI agent, memory is **what makes me more than stateless**. Without it, every task starts from zero. With it, I accumulate knowledge, learn procedures, and remember past interactions.

---

## Verdict: STRONG — the best-designed subsystem for agentic workflows

The memory system is the most agent-friendly part of AgentOS. Three tiers with distinct purposes, hybrid search (vector + FTS5), proper isolation, and rich tool coverage. Some gaps exist in auto-population and cross-task knowledge transfer.

---

## Findings

### 1. Semantic Memory — EXCELLENT

**Architecture:**
- SQLite + FTS5 + vector embeddings (fastembed, 384 dimensions).
- Text is chunked (2000 chars, 200 overlap) before embedding.
- Hybrid search: FTS5 pre-filter (200 candidates) → cosine similarity → RRF fusion.
- Each entry has: id, agent_id, key, content, created_at, updated_at, tags.

**What works well for me as an agent:**
- **Hybrid search** — FTS5 catches exact keyword matches, vectors catch semantic similarity. I don't need to know the exact words used.
- **min_score parameter** — I can filter low-quality results (default 0.3).
- **Tags** — I can organize knowledge by topic.
- **Key-based lookup** — `get_by_key()` for when I know exactly what I want.
- **Export/import JSONL** — memory is portable across instances.
- **Sweep old entries** — archival garbage collection.
- **Transactional writes** — no partial writes on crash.
- **FTS5 injection prevention** — queries are wrapped in quotes, double-quotes escaped.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 1 | `std::sync::Mutex` instead of `tokio::sync::Mutex` — blocks the async runtime during lock contention | High | Under load, memory operations can block other tasks |
| 2 | No upsert by key — writing twice with the same key creates duplicates | Medium | I must check-then-write to avoid duplicate entries |
| 3 | Embedding model (384-dim) is hardcoded — can't use a better model without code changes | Low | Future concern for model upgrades |
| 4 | `cosine_similarity` is O(n) per chunk — with 500+ chunks in recency fallback, this is slow | Medium | Search latency scales linearly with memory size |
| 5 | No pagination in search results — `top_k` is the only control | Low | For large result sets, I can't page through |

### 2. Episodic Memory — SOLID

**Architecture:**
- SQLite + FTS5 (no vector embeddings — text search only).
- Task-scoped: every entry tied to `task_id` and `agent_id`.
- Entry types: UserPrompt, LLMResponse, ToolCall, ToolResult, SystemEvent, Error, Reflection.
- Task ownership enforcement: `recall_task()` checks agent ownership.

**What works well for me as an agent:**
- **Task-scoped isolation** — my episodes don't mix with other agents' episodes.
- **Ownership check** — other agents can't read my task history without permission.
- **Timeline retrieval** — `timeline_by_task()` gives chronological history.
- **Global search** — `recall_global()` lets me search across all tasks with proper scoping.
- **BM25 ranking** — FTS5 returns relevance-ranked results.
- **Export/import JSONL** — portable episodic data.
- **`find_successful_episodes()`** — used by consolidation to extract procedures from success patterns.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 6 | `std::sync::Mutex` same as semantic — blocks async runtime | High | Same contention issue |
| 7 | No vector search — only FTS5 text search, so synonym-based queries fail | Medium | "deployment" won't find entries about "release process" |
| 8 | `task_history()` has a hardcoded `LIMIT 10000` — very long tasks silently lose older entries | Medium | Long-running autonomous tasks may lose early context |
| 9 | `find_successful_episodes()` uses `LIKE '%"outcome":"success"%'` — fragile JSON string matching | Medium | Breaks if metadata format changes slightly |
| 10 | No summarization — I get raw entries, not condensed summaries of past tasks | High | Large episodic history wastes context tokens |

### 3. Procedural Memory — GOOD

**Architecture:**
- SQLite + FTS5 + vector embeddings (same 384-dim model).
- Each procedure has: name, description, preconditions, steps (ordered actions with tool references), postconditions, success/failure counts, source episodes.
- Search: hybrid FTS5 + cosine similarity with RRF fusion (same as semantic).
- Success/failure tracking via `update_stats()`.

**What works well for me as an agent:**
- **Structured procedures** — preconditions, ordered steps, postconditions. This is exactly what I need to execute multi-step tasks.
- **Tool references in steps** — each step can reference the tool to use, so I know what permissions I need.
- **Success/failure tracking** — I can prefer procedures with high success rates.
- **Semantic search** — I can find procedures by describing what I want to accomplish.
- **CREATE/DELETE/LIST/SEARCH tools** — full CRUD for procedures.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 11 | `store()` uses manual `BEGIN/COMMIT TRANSACTION` instead of `conn.transaction()` — inconsistent with semantic store's pattern, and if an error occurs between BEGIN and COMMIT, the `ROLLBACK` may not fire | Medium | Potential for partial writes on edge case failures |
| 12 | Procedures are not auto-populated — I must manually create them or rely on consolidation | Medium | Procedures don't accumulate organically |
| 13 | No procedure versioning — updating a procedure replaces it (INSERT OR REPLACE) | Low | Can't roll back a bad procedure update |
| 14 | `search()` FTS path interpolates rowids directly into SQL — though they come from a prior SQLite query (not user input), this differs from semantic store's parameterized approach | Low | Inconsistent security pattern (not exploitable, but divergent) |

### 4. Memory Block System — SIMPLE AND EFFECTIVE

File-based key-value storage in `data_dir/memory_blocks/`. Good for structured data that doesn't need search.

**What works well:** Simple CRUD, no dependencies on embedding model, good for configuration and state.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 15 | No atomic writes — if the process crashes mid-write, blocks can be corrupted | Medium | Should use tmp+rename pattern like file-editor |
| 16 | No size limit on memory blocks — I could accidentally write a huge block | Low | Could be protected by a size guard |

### 5. Cross-Cutting Issues

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 17 | **No auto-write on task completion** — the architecture describes this but it's not implemented. Episodic memory must be explicitly written by tools | High | I must remember to save my experiences manually |
| 18 | **No cross-task knowledge transfer** — semantic memory is the only shared tier, but there's no automatic mechanism to promote episodic insights to semantic facts | High | Insights from one task are lost unless I manually copy them |
| 19 | **Consolidation engine is mentioned but not wired** — `find_successful_episodes()` exists but no scheduler calls it | High | Procedures don't auto-extract from successful task patterns |
| 20 | **All three stores use std::sync::Mutex** — this means any memory operation holds a non-async lock, blocking the entire tokio runtime thread during database access | High | Under concurrent agent load, memory becomes a bottleneck |

---

## Critical Gaps for Pure Agentic Workflow

### Gap A: No Automatic Episodic Memory on Task Completion

When I finish a task, nothing is automatically recorded to episodic memory. This means:
- My experiences are lost unless I explicitly call `memory-write` before the task ends.
- If the task fails/times out, there's no record of what happened.
- The consolidation engine has no data to work with.

**Recommendation:** Add a kernel hook in `task_completion.rs` that auto-writes a summary to episodic memory when a task reaches a terminal state (Complete, Failed, Cancelled).

### Gap B: No Memory Summarization

When I search episodic memory and get 20 entries from a past task, I have to spend context tokens reading raw events. There's no way to ask "summarize what happened in task X."

**Recommendation:** Add a `memory-summarize` tool that takes a task_id and returns a condensed summary (or store summaries automatically on task completion).

### Gap C: Blocking Mutex Contention

All three memory stores use `std::sync::Mutex`. In an async runtime serving multiple agents:
1. Agent A writes to semantic memory → locks Mutex.
2. Agent B searches semantic memory → blocks the tokio worker thread.
3. All other async tasks on that thread are delayed.

**Recommendation:** Use `tokio::sync::Mutex` or wrap database operations in `tokio::task::spawn_blocking()`.

---

## Test Coverage Assessment

| Module | Unit Tests | Coverage Quality |
|--------|-----------|-----------------|
| semantic.rs | 1 test | **Minimal** — only search ranking tested |
| episodic.rs | 3 tests | Good — record, FTS, ownership denial |
| procedural.rs | 3 tests | Good — store, search, stats/delete |
| embedder.rs | Not audited | — |
| types.rs | Not audited | — |

**Recommendation:** Add tests for:
- Semantic write + search roundtrip with tags
- Semantic delete + verify FTS index cleanup
- Episodic sweep_old_entries
- Procedural list_by_agent
- Concurrent access patterns (requires async test)

---

## Score

| Criterion | Score (1-5) | Notes |
|-----------|------------|-------|
| Completeness | 3.5 | Three tiers exist but auto-population is missing |
| Correctness | 4.0 | Proper transactions, parameterized queries, FTS injection prevention |
| Agent Ergonomics | 4.0 | Rich tool suite, hybrid search, structured procedures |
| Performance | 2.5 | Blocking mutexes, no pagination, linear cosine scan |
| Reliability | 3.5 | Memory blocks lack atomic writes; inconsistent transaction patterns |
| **Overall** | **3.5/5** | Excellent design, needs operational hardening |
