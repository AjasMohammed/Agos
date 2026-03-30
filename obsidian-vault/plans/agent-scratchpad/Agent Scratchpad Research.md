---
title: Agent Scratchpad Research
tags:
  - memory
  - scratchpad
  - v3
  - research
date: 2026-03-23
status: complete
effort: 1d
priority: high
---

# Agent Scratchpad Research

> Research synthesis on why agents need unstructured working memory and how existing systems compare.

---

## The Problem: Structured Memory Is Not Thinking Memory

Current AgentOS memory tiers are designed for **recall**, not **reasoning**:

| Tier | Purpose | Metaphor | Limitation for "Thinking" |
|------|---------|----------|--------------------------|
| Episodic | What happened | Diary | Append-only, can't edit or restructure |
| Semantic | What I know | Encyclopedia | Indexed by embedding similarity, not association |
| Procedural | How to do X | Recipe book | Rigid schema, not freeform |
| Memory Blocks | Temporary notes | Post-it notes | 2KB limit, no linking, flat key-value |

None of these support the **associative, iterative, non-linear** nature of working memory. When an agent is:
- Investigating a bug across multiple files — it needs to link observations
- Planning a multi-step task — it needs to draft, revise, and cross-reference
- Learning about a new codebase — it needs to build a concept map organically

...it has nowhere to put this intermediate knowledge in a way that's both persistent and navigable.

---

## Prior Art: What Makes Obsidian Work

Obsidian's power comes from a small set of primitives:

1. **Local-first markdown files** — no schema, no structure imposed; the file is the unit of thought
2. **Wikilinks `[[Page Name]]`** — zero-friction linking creates a graph naturally as you write
3. **Backlinks** — every page shows what links TO it, enabling serendipitous discovery
4. **Graph view** — visual navigation of the knowledge topology
5. **Tags and frontmatter** — lightweight metadata without schema constraints

For agents, we don't need #4 (visual graph) or the full plugin ecosystem. We need:
- **Markdown pages** (items 1)
- **Wikilink resolution** (item 2)
- **Backlink index** (item 3)
- **Tag/frontmatter filtering** (item 5)
- **Graph traversal for context injection** (agent-specific need — inject related notes into LLM context)

---

## Existing MemoryBlockStore Analysis

The current `MemoryBlockStore` in `crates/agentos-kernel/src/memory_blocks.rs`:

```rust
pub struct MemoryBlock {
    pub id: String,        // UUID
    pub agent_id: String,
    pub label: String,     // Unique per agent — the "key"
    pub content: String,   // Max 2048 bytes
    pub created_at: String,
    pub updated_at: String,
}
```

**Schema:**
```sql
CREATE TABLE memory_blocks (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    label TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(agent_id, label)
)
```

**Limitations:**
- 2KB content limit — can't store meaningful analysis
- No full-text search — only exact label lookup
- No linking between blocks — flat namespace
- No metadata/tags — just label + content
- No cross-agent visibility
- No graph structure

**What to keep:** The dispatch pattern (tools return `_kernel_action`, kernel handles storage) works well and should be reused for scratchpad tools.

---

## Design Principles for Agent Scratchpad

### 1. Markdown is the format
Agents already generate markdown naturally. Don't invent a custom format. Store raw markdown, parse wikilinks out of it.

### 2. Wikilinks are the graph edges
`[[Page Title]]` in content creates a directed edge from current page to target page. This is the only linking mechanism — no explicit "link" API needed.

### 3. Titles are the namespace
Pages are identified by title within an agent's namespace. Titles must be unique per agent. Cross-agent references use `@agent_id/Page Title` syntax.

### 4. The backlink index is the killer feature
Every page knows what links to it without scanning all content. This enables:
- "What else is related to this concept?" (backlink query)
- "Inject related context" (walk the graph from current topic)
- "Find orphan pages" (pages with no inbound links)

### 5. Graph traversal serves context injection
The primary consumer of the graph is the `ContextManager`. Before an LLM call, it can:
1. Identify the current task's topic
2. Walk the scratchpad graph from that topic (BFS, depth 2)
3. Inject the most relevant linked pages into the context window
4. This is automatic associative recall — the agent doesn't need to explicitly search

### 6. Size limits prevent abuse
- 64KB per page (enough for detailed notes, not enough for data dumps)
- 1000 pages per agent (prevents unbounded growth)
- 8KB total injection budget for context (prevents context window flooding)

---

## Comparison: Scratchpad vs Existing Tiers

| Feature | Scratchpad | Semantic | Episodic | Blocks |
|---------|-----------|----------|----------|--------|
| Content format | Markdown | Structured entry | Structured entry | Plain text |
| Max size | 64KB | Unlimited (chunked) | Unlimited | 2KB |
| Linking | Wikilinks + backlinks | Embedding similarity | Timeline ordering | None |
| Search | FTS5 + tag filter | Hybrid (cosine + FTS) | FTS5 + type filter | Label exact match |
| Editability | Full CRUD | Full CRUD | Append-only | Full CRUD |
| Graph structure | Yes (adjacency) | No (vector space) | No (linear) | No |
| Context injection | Graph-aware BFS | Similarity search | Recency-based | Manual |
| Cross-agent | With capability token | No | No | No |

---

## Technology Choices

| Choice | Decision | Rationale |
|--------|----------|-----------|
| Storage backend | SQLite (new DB file) | Consistent with all AgentOS persistence; FTS5 for free |
| Wikilink parsing | Regex `\[\[([^\]]+)\]\]` | Simple, fast, no dependencies needed |
| Graph representation | Adjacency table in SQLite | Scales well, supports SQL-based traversal queries |
| Text search | FTS5 virtual table | Already used in episodic and procedural stores |
| Crate location | New `agentos-scratch` | Single responsibility; clean dependency boundary |
| Async model | `spawn_blocking` for SQLite | Consistent with existing stores; WAL mode for concurrency |

---

## Related

- [[Agent Scratchpad Plan]]
- [[Agent Scratchpad Data Flow]]
- [[Memory System]]
