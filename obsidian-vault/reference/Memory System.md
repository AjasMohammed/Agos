---
title: Memory System
tags: [reference, memory]
---

# Memory System

AgentOS provides a dual-mode memory system with semantic (vector) and episodic (time-series) stores.

**Source:** `crates/agentos-memory/src/`

## Architecture

```
┌──────────────────────────────────────────────┐
│              Memory System                    │
│                                              │
│  ┌─────────────────┐  ┌──────────────────┐  │
│  │ Semantic Store   │  │ Episodic Store   │  │
│  │                  │  │                  │  │
│  │ - Vector search  │  │ - Time-series    │  │
│  │ - FTS5 hybrid    │  │ - FTS5 keyword   │  │
│  │ - Embeddings     │  │ - Per-agent      │  │
│  │ - Chunking       │  │ - Tagging        │  │
│  └────────┬─────────┘  └──────────────────┘  │
│           │                                   │
│  ┌────────┴─────────┐                        │
│  │ Embedder          │                        │
│  │ MiniLM-L6-v2      │                        │
│  │ (ONNX Runtime)    │                        │
│  │ 384-dim vectors   │                        │
│  └──────────────────┘                         │
└──────────────────────────────────────────────┘
```

## Semantic Memory

### Purpose
Long-term knowledge storage with vector similarity search. Used when agents need to recall information by meaning, not exact keywords.

### Storage Schema (SQLite)

```sql
-- Main entries
semantic_memory (id, agent_id, key, content, created_at, updated_at, tags)

-- Chunked content with embeddings
semantic_chunks (id, memory_id, chunk_index, content, embedding BLOB)

-- Full-text search index
semantic_fts (FTS5 virtual table on chunk content)
```

### Search

Hybrid search combining:
1. **Vector similarity** - Cosine similarity between query embedding and stored embeddings
2. **FTS5 keyword search** - SQLite full-text search for exact matches
3. Results ranked by combined score

### Write

Content is:
1. Chunked into manageable pieces
2. Each chunk embedded via MiniLM-L6-v2 (384-dim vector)
3. Stored with metadata (agent_id, key, tags)

## Episodic Memory

### Purpose
Time-ordered event log for agents. Records what happened, when, and in what context.

### Storage Schema (SQLite)

```sql
episodic_memory (id, agent_id, content, timestamp, scope, summary, tags)
```

### Search
- FTS5 full-text keyword search
- Filter by agent, time range, scope, tags

## Embedder

- **Model:** all-MiniLM-L6-v2 (~23MB)
- **Runtime:** ONNX Runtime (CPU, no GPU required)
- **Output:** 384-dimensional f32 vectors
- **Caching:** Model cached locally in configurable directory
- **Lazy loading:** Model loaded on first use

## Tools Interface

Agents interact with memory through two built-in tools:

### memory-search
```json
{
  "query": "search terms",
  "store": "semantic",       // or "episodic"
  "agent_id": "optional",
  "limit": 10
}
```

### memory-write
```json
{
  "key": "entry-name",
  "content": "content to store",
  "store": "semantic",       // or "episodic"
  "tags": ["tag1", "tag2"]
}
```

## Memory Tiers

| Tier | Store | Lifespan | Use Case |
|---|---|---|---|
| Working | Context Window | Per-task | Current conversation |
| Episodic | EpisodicStore | Persistent | Task history, events |
| Semantic | SemanticStore | Persistent | Knowledge, facts, documents |
