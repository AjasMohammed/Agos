---
title: Agent Scratchpad Data Flow
tags:
  - memory
  - scratchpad
  - v3
  - flow
date: 2026-03-23
status: planned
effort: 1h
priority: high
---

# Agent Scratchpad Data Flow

> How data flows through the scratchpad system — from agent write to graph-aware context injection.

---

## Write Flow

```
Agent (via LLM tool call)
  │
  ▼
┌─────────────────────┐
│ scratch-write tool   │
│ (agentos-tools)      │
│                     │
│ Input:              │
│  - title: String    │
│  - content: String  │
│  - tags: Vec<String>│
│                     │
│ Returns:            │
│  _kernel_action:    │
│  "scratch_write"    │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│ Kernel Dispatch      │
│ (task_executor.rs)   │
│                     │
│ Matches action →    │
│ routes to           │
│ ScratchpadStore     │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────────────────────────┐
│ ScratchpadStore::write_page()           │
│ (agentos-scratch/src/store.rs)          │
│                                         │
│ 1. Validate: title length, content size │
│ 2. Parse frontmatter (if present)       │
│ 3. INSERT OR REPLACE into pages table   │
│ 4. Update FTS5 index                    │
│ 5. Parse [[wikilinks]] from content     │
│ 6. Update link_index table:             │
│    - DELETE old outbound links           │
│    - INSERT new outbound links           │
│ 7. Return PageID                        │
└─────────────────────────────────────────┘
```

## Read Flow

```
Agent (via LLM tool call)
  │
  ▼
┌─────────────────────┐
│ scratch-read tool    │
│                     │
│ Input:              │
│  - title: String    │
│                     │
│ Returns:            │
│  _kernel_action:    │
│  "scratch_read"     │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────────────────────────┐
│ ScratchpadStore::read_page()            │
│                                         │
│ 1. SELECT * FROM pages                  │
│    WHERE agent_id = ? AND title = ?     │
│ 2. Return page content + metadata       │
│ 3. Optionally: resolve [[wikilinks]]    │
│    to indicate which targets exist       │
└─────────────────────────────────────────┘
```

## Search Flow

```
Agent (via LLM tool call)
  │
  ▼
┌─────────────────────┐
│ scratch-search tool  │
│                     │
│ Input:              │
│  - query: String    │
│  - tags: Vec<String>│
│  - limit: usize     │
│                     │
│ Returns:            │
│  _kernel_action:    │
│  "scratch_search"   │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────────────────────────┐
│ ScratchpadStore::search()               │
│                                         │
│ 1. FTS5 query on pages_fts             │
│ 2. Optional tag filter via metadata     │
│ 3. Rank by BM25 score                  │
│ 4. Return top-N results with snippets  │
└─────────────────────────────────────────┘
```

## Backlink / Graph Query Flow

```
Agent (via LLM tool call)
  │
  ▼
┌─────────────────────┐
│ scratch-links tool   │
│                     │
│ Input:              │
│  - title: String    │
│  - direction:       │
│    "inbound" |      │
│    "outbound" |     │
│    "both"           │
│                     │
│ Returns:            │
│  _kernel_action:    │
│  "scratch_links"    │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────────────────────────┐
│ ScratchpadStore::get_links()            │
│                                         │
│ Inbound (backlinks):                    │
│   SELECT source_title FROM link_index   │
│   WHERE target_title = ? AND agent_id=? │
│                                         │
│ Outbound (forward links):               │
│   SELECT target_title FROM link_index   │
│   WHERE source_title = ? AND agent_id=? │
│                                         │
│ Returns list of linked page titles      │
└─────────────────────────────────────────┘
```

## Context Injection Flow (Automatic)

```
┌─────────────────────────────────────────────────────────┐
│ Task Execution (before LLM inference call)               │
│                                                          │
│ 1. ContextManager identifies current task topic          │
│    (from task description, recent tool results, etc.)    │
│                                                          │
│ 2. Check if agent has scratchpad pages matching topic    │
│    - FTS5 search on topic keywords                       │
│    - OR: explicit page title from task metadata          │
│                                                          │
│ 3. If matches found, invoke GraphWalker:                │
│    ┌───────────────────────────────────────────┐        │
│    │ GraphWalker::subgraph(start_page, depth=2)│        │
│    │                                           │        │
│    │ BFS traversal:                            │        │
│    │   depth 0: start_page                     │        │
│    │   depth 1: pages linked FROM start_page   │        │
│    │            + pages linking TO start_page   │        │
│    │   depth 2: neighbors of depth-1 pages     │        │
│    │                                           │        │
│    │ Filters:                                  │        │
│    │   - visited set (no cycles)               │        │
│    │   - max_pages (default 5)                 │        │
│    │   - max_total_bytes (default 8KB)         │        │
│    │                                           │        │
│    │ Returns: Vec<ScratchPage> ordered by       │        │
│    │          relevance (distance from start)   │        │
│    └───────────────────────────────────────────┘        │
│                                                          │
│ 4. Inject pages as ContextEntry::ScratchpadNote          │
│    into the context window (before the LLM call)         │
│                                                          │
│ 5. LLM sees related scratchpad notes as context          │
│    → can reference, update, or create new links          │
└─────────────────────────────────────────────────────────┘
```

## Cross-Agent Read Flow

```
Agent A wants to read Agent B's scratchpad
  │
  ▼
┌─────────────────────────────────────────┐
│ scratch-read tool                        │
│ Input: title = "@agent_b_id/Page Title" │
└─────────┬───────────────────────────────┘
          │
          ▼
┌─────────────────────────────────────────┐
│ Kernel Dispatch                          │
│                                         │
│ 1. Parse @agent_id prefix               │
│ 2. Check CapabilityToken for:           │
│    Permission: "scratchpad:read:<b_id>" │
│ 3. If authorized → read from B's store  │
│ 4. If denied → return PermissionDenied  │
└─────────────────────────────────────────┘
```

## SQLite Schema

```sql
-- Main pages table
CREATE TABLE pages (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    metadata TEXT,          -- JSON: parsed frontmatter
    tags TEXT,              -- JSON array for quick filtering
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(agent_id, title)
);

-- FTS5 full-text search index
CREATE VIRTUAL TABLE pages_fts USING fts5(
    title, content, tags,
    content='pages',
    content_rowid='rowid'
);

-- Link adjacency index (eager, updated on write)
CREATE TABLE link_index (
    source_id TEXT NOT NULL REFERENCES pages(id),
    target_title TEXT NOT NULL,  -- may reference non-existent page
    agent_id TEXT NOT NULL,
    link_text TEXT NOT NULL,     -- the raw [[text]] as written
    UNIQUE(source_id, target_title)
);

CREATE INDEX idx_links_target ON link_index(agent_id, target_title);
CREATE INDEX idx_links_source ON link_index(agent_id, source_id);
CREATE INDEX idx_pages_agent ON pages(agent_id);
```

---

## Steps Walkthrough

1. **Agent calls `scratch-write`** with title, content, optional tags
2. **Tool returns kernel action** — no direct DB access from tool
3. **Kernel dispatches** to `ScratchpadStore::write_page()`
4. **Store validates** content size (<=64KB), title length, page count (<=1000/agent)
5. **Store parses wikilinks** via regex `\[\[([^\]]+)\]\]`
6. **Store updates** pages table + FTS5 index + link_index in a single transaction
7. **On next LLM inference**, `ContextManager` checks if task has relevant scratchpad context
8. **GraphWalker** traverses the link graph from relevant pages (BFS, depth 2)
9. **Related pages** are injected into context window as `ContextEntry::ScratchpadNote`
10. **LLM sees** the related notes and can reference or extend them

---

## Related

- [[Agent Scratchpad Plan]]
- [[Agent Scratchpad Research]]
