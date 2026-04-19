---
title: Hybrid Tool Selection
tags:
  - kernel
  - tools
  - performance
  - llm
  - plan
date: 2026-04-18
status: in-progress
effort: 2d
priority: high
---

# Hybrid Tool Selection Plan

> Reduce LLM context cost by injecting only the tools relevant to a task, using a three-layer hybrid filter instead of dumping all 104+ registered tools on every call.

---

## Problem

Every LLM call today sends the full tool manifest list (`llm_tool_manifests` in `task_executor.rs:2185`). With 104 core tools + MCP-registered tools, this costs **2,000–5,000 tokens per iteration** — paid 10× in a typical task. The only existing filter is `CapabilityToken.allowed_tools` (a BTreeSet of tool IDs), which is rarely populated, so most tasks get everything.

## Target Architecture

Three-layer hybrid filter applied once per task before the agent loop:

```
All registered tools
  ↓ Layer 1: Permission filter      (hard gate — remove uncallable tools)
  ↓ Layer 2: Always-on partition    (core tools always included)
  ↓ Layer 3a: Group detection       (keyword signals → whole group included)
  ↓ Layer 3b: Semantic ranking      (embed task prompt, cosine-sim top-K from remainder)
  → Final set: always-on + group-match + semantic-top-K  (capped at max_total)
```

## Phase Overview

| Phase | Name | Effort | Status |
|-------|------|--------|--------|
| 1 | Tool group tagging | 2h | in-progress |
| 2 | ToolSelectionConfig | 30m | in-progress |
| 3 | ToolSelector module | 3h | in-progress |
| 4 | Kernel + executor wiring | 1h | in-progress |

## Key Design Decisions

1. **`group` field on ToolInfo** — single string, defaults to `"misc"`, read from TOML manifest `[manifest] group = "..."`. Twelve groups: `core`, `fs`, `memory`, `network`, `process`, `coordination`, `events`, `comms`, `hal`, `iot`, `container`, `kmc`.

2. **Always-on tools** — configurable list of tool names always injected if permitted. Defaults: `think`, `agent-self`, `agent-manual`, `context-memory-read`, `context-memory-update`, `memory-search`, `memory-write`. Memory tools are always-on because memory is part of the agent's identity.

3. **Group detection is keyword-based** — fast, zero-cost, no model needed. Signals are conservative (false-positives are ok; false-negatives waste tokens but don't break tasks).

4. **Semantic ranking uses `agentos_memory::Embedder`** — reuses the existing MiniLM model already loaded for memory operations. Tool embeddings are pre-computed once and cached in a `RwLock<HashMap>`. Gracefully degrades to keyword scoring if model unavailable.

5. **Hard cap `max_total_tools = 40`** — enforced after merge. If selection produces fewer than `min_tools` (default 8), fill from permission-filtered set to guarantee minimum coverage.

6. **Selection disabled per-task** — `AgentTask` can set `skip_tool_selection: bool` to bypass (used for tool-discovery tasks).

## Expected Savings

| Scenario | Before | After |
|----------|--------|-------|
| Simple file task | 104 tools injected | ~15 tools (core + fs + semantic) |
| Web research task | 104 tools | ~18 tools (core + network + memory) |
| Multi-agent orchestration | 104 tools | ~22 tools (core + coordination + memory) |
| HAL/hardware task | 104 tools | ~20 tools (core + hal + iot) |
| General task (worst case) | 104 tools | ≤40 tools (capped) |
