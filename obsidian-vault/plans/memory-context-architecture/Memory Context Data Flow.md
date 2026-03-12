---
title: Memory & Context Data Flow
tags:
  - flow
  - memory
  - context
  - kernel
date: 2026-03-12
status: planned
effort: 14d
priority: critical
---

# Memory & Context Data Flow

> How data flows through the tiered memory architecture, from user input through retrieval, context compilation, inference, and memory consolidation.

---

## Diagram

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          AGENT RUN LOOP                                 │
│                                                                         │
│  ┌──────────┐    ┌──────────────────┐    ┌──────────────────────────┐   │
│  │  User     │───▶│  Retrieval Gate  │───▶│   Context Compiler       │   │
│  │  Input    │    │                  │    │                          │   │
│  └──────────┘    │  Classifies:     │    │  Allocates token budget: │   │
│                  │  - Skip retrieval │    │  ┌────────────────────┐  │   │
│                  │  - Episodic       │    │  │ System (12%)       │  │   │
│                  │  - Semantic       │    │  │ Tools  (18%)       │  │   │
│                  │  - Procedural     │    │  │ Knowledge (35%)    │  │   │
│                  │  - Tools          │    │  │ History (20%)      │  │   │
│                  └────────┬─────────┘    │  │ Task    (10%)      │  │   │
│                           │              │  │ Reserve (25%)      │  │   │
│                  ┌────────▼─────────┐    │  └────────────────────┘  │   │
│                  │  Multi-Index     │    │                          │   │
│                  │  Retrieval       │───▶│  Position-aware:        │   │
│                  │                  │    │  [System → Knowledge →  │   │
│                  │  Parallel query  │    │   History → Task]       │   │
│                  │  + RRF fusion    │    └────────────┬─────────────┘   │
│                  └──────────────────┘                 │                 │
│                                                      ▼                 │
│                                           ┌──────────────────┐         │
│                                           │  LLM Inference   │         │
│                                           │  (ContextWindow)  │         │
│                                           └────────┬─────────┘         │
│                                                    │                   │
│                            ┌───────────────────────┼──────────────┐    │
│                            ▼                       ▼              ▼    │
│                  ┌─────────────────┐    ┌──────────────┐  ┌────────┐  │
│                  │ Tool Execution  │    │  Response to  │  │ Memory │  │
│                  │ (if tool call)  │    │  User         │  │Pipeline│  │
│                  └────────┬────────┘    └──────────────┘  │(async) │  │
│                           │                               └───┬────┘  │
│                           ▼                                   │       │
│                  ┌─────────────────┐                          │       │
│                  │ Push tool result│                          │       │
│                  │ to context      │                          │       │
│                  └─────────────────┘                          │       │
│                                                              │       │
└──────────────────────────────────────────────────────────────┼───────┘
                                                               │
                              BACKGROUND TASKS                 │
┌──────────────────────────────────────────────────────────────┼───────┐
│                                                              ▼       │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │                    Memory Pipeline                             │  │
│  │                                                                │  │
│  │  1. Extract facts (LLM-based)                                 │  │
│  │     "User prefers JSON output" → MemoryCandidate               │  │
│  │                                                                │  │
│  │  2. Conflict detection (cosine > 0.85)                        │  │
│  │     Compare against existing semantic memories                 │  │
│  │                                                                │  │
│  │  3. Apply operation                                           │  │
│  │     ADD → new semantic entry                                   │  │
│  │     UPDATE → merge into existing                               │  │
│  │     DELETE → remove contradicted                               │  │
│  │     NOOP → already known                                       │  │
│  └────────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │                 Consolidation Engine                            │  │
│  │                 (periodic: every 100 tasks or 24h)             │  │
│  │                                                                │  │
│  │  1. Query episodic store for similar successful patterns       │  │
│  │     (group by embedding similarity, min 3 occurrences)        │  │
│  │                                                                │  │
│  │  2. Distill via LLM into Procedure                            │  │
│  │     EpisodicPattern → Procedure(name, steps, conditions)      │  │
│  │                                                                │  │
│  │  3. Store in ProceduralStore                                   │  │
│  │     (available for future retrieval gate queries)              │  │
│  └────────────────────────────────────────────────────────────────┘  │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

## Memory Tier Interactions

```
┌─────────────────────────────────────────────────────────────────┐
│                    MEMORY ARCHITECTURE                           │
│                                                                  │
│  ┌──────────────┐   compile()   ┌──────────────────────────┐    │
│  │ Working      │◀──────────────│ Context Compiler          │    │
│  │ Memory       │               │ (assembles per invocation)│    │
│  │ (context     │               └──────────────────────────┘    │
│  │  window)     │                    ▲   ▲   ▲                  │
│  └──────────────┘                    │   │   │                  │
│                                      │   │   │                  │
│         search()  ┌──────────────────┘   │   └────────────┐     │
│                   │                      │                │     │
│  ┌────────────────▼──┐  ┌───────────────▼──┐  ┌─────────▼──┐  │
│  │ Semantic Store     │  │ Episodic Store   │  │ Procedural │  │
│  │                    │  │                  │  │ Store      │  │
│  │ • Facts/knowledge  │  │ • Task events    │  │            │  │
│  │ • FTS5 + cosine    │  │ • Conversations  │  │ • Skills   │  │
│  │ • RRF fusion       │  │ • Tool results   │  │ • SOPs     │  │
│  │ • Chunked (2K/200) │  │ • Outcomes       │  │ • Patterns │  │
│  │ • SQLite-backed    │  │ • SQLite-backed  │  │ • SQLite   │  │
│  └────────────────────┘  └────────┬─────────┘  └─────▲─────┘  │
│           ▲                       │                   │         │
│           │ ADD/UPDATE/DELETE      │  consolidate()    │         │
│           │                       └───────────────────┘         │
│  ┌────────┴───────────┐                                         │
│  │  Memory Pipeline   │                                         │
│  │  (fact extraction) │                                         │
│  └────────────────────┘                                         │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ Tool Registry (Name-Based Lookup)                         │   │
│  │                                                           │   │
│  │ get_by_name(name) → exact string match                    │   │
│  │ list_all() → all registered tools                         │   │
│  │ tools_for_prompt() → all tools formatted for LLM          │   │
│  │ NOTE: Vector search deferred to Phase 2 (V3.3+, ~30+     │   │
│  │ tools needed to justify embedding model overhead)          │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

## Steps

### 1. User Input Arrives
User prompt enters the run loop. Before any LLM call, the system decides what context to assemble.

### 2. Retrieval Gate Classifies
`RetrievalGate::classify()` examines the query using keyword heuristics to produce a `RetrievalPlan` — which indexes to query and how many results per index. Simple follow-ups skip retrieval entirely.

### 3. Multi-Index Retrieval
Based on the plan, queries run in parallel against selected stores (semantic, episodic, procedural, tool registry). Results are fused using Reciprocal Rank Fusion (RRF) with configurable weights.

### 4. Context Compilation
`ContextCompiler::compile()` builds a fresh `ContextWindow`:
- Calculates token budget per category (system, tools, knowledge, history, task)
- Subtracts 25% reserve for output tokens
- Places system instructions at position 0 (primacy effect)
- Places current task at the end (recency effect)
- Fills knowledge and history in middle positions
- Truncates or summarizes categories that exceed budget

### 5. LLM Inference
The compiled `ContextWindow` is passed to `LLMCore::infer()`. The window contains only what's needed — no bloated tool lists, no stale history, no irrelevant memories.

### 6. Tool Execution (if tool call)
If the LLM requests a tool call, the tool executes with capability validation. Results are pushed to context with sanitization and taint wrapping.

### 7. Memory Pipeline (async)
After tool results are processed, a background task extracts salient facts from the conversation, detects conflicts with existing memories, and applies ADD/UPDATE/DELETE operations to the semantic store.

### 8. Episodic Recording
On task completion, an episodic entry is recorded with outcome, duration, tools used, and cost metadata.

### 9. Consolidation (periodic)
Every 100 task completions (or 24h), the consolidation engine scans episodic entries for repeated successful patterns, distills them into procedures via LLM, and stores them in the procedural store for future retrieval.

## Related

- [[Memory Context Architecture Plan]] — master plan and design decisions
- [[Memory Context Research Synthesis]] — source research synthesis
