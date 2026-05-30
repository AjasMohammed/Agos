# Memory

AgentOS provides a multi-tier memory system: persistent SQLite stores plus structured memory
blocks and archival storage, unified by hybrid FTS5 + vector retrieval with reciprocal-rank
fusion (RRF).

## Tiers

| Tier | Store | Lifespan | Purpose |
|------|-------|----------|---------|
| Working | `ContextWindow` | per-task | Current conversation, in-task scratch. |
| Memory blocks | `agent_blocks.db` | persistent | Structured per-agent key-value state (role + token budget). |
| Episodic | `episodic_memory.db` | persistent | Time-ordered task events; auto-written on task completion. |
| Semantic | `semantic_memory.db` | persistent | Long-term facts and documents (embedded + chunked). |
| Procedural | `procedural_memory.db` | persistent | Distilled patterns/procedures (output of consolidation). |
| Archival | within the semantic store | persistent | Large reference dumps; high-recall search. |

The three primary stores (semantic, episodic, procedural) share the FTS5 + cosine + RRF
retrieval pipeline. Embeddings use fastembed MiniLM-L6-v2 (384-dim).

## How memory fills and is used

- **Episodic auto-write** — when a task completes, a summary of its events is written to
  episodic memory (`task_completion.rs`).
- **Memory extraction** — facts are mined from tool results and written to semantic memory
  (`memory_extraction.rs`), gated by `[memory.extraction]` (conflict threshold, facts per
  result, min result length).
- **Consolidation** — the consolidation engine distills recurring episodic patterns into
  reusable procedures (`consolidation.rs`), triggered by `[memory.consolidation]` (occurrence
  count, completion/time triggers).
- **Retrieval gate** — a multi-tier search across all stores assembles knowledge blocks into
  the context window (`retrieval_gate.rs`).

## Configuration

```toml
[memory]
model_cache_dir = "models"        # embedding model cache

[memory.extraction]
enabled = true
conflict_threshold = 0.85
max_facts_per_result = 5
min_result_length = 50

[memory.consolidation]
enabled = true
min_pattern_occurrences = 3
task_completions_trigger = 100
time_trigger_hours = 24

[memory.context]
enabled = true                    # per-agent self-curated context memory
max_tokens = 4096
db_path = "context_memory.db"
```

The `[context_budget]` section governs how the available token budget is split across system
prompt, tools, knowledge, history, and the task itself.

## Tools

Agents interact with memory through tools (all gated by `memory.*` permissions):
`memory-search`, `memory-write`, `memory-read`, `memory-stats`, the `memory-block-*` named
blocks, `archival-insert`/`archival-search`, and the `procedure-*` family. See
[Tools](./tools.md).
