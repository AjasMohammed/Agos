---
title: Capability Discoverability Plan
tags:
  - kernel
  - tools
  - agents
  - v4
  - plan
date: 2026-04-07
status: partial
effort: 2d
priority: high
---

# Capability Discoverability Plan

> Improve how agents discover what they can do — the hidden problem behind multiple suggestions from the agent feedback session.

---

## Why This Matters

When an agent was asked "what would improve the ecosystem?", it requested features that **already exist**: ephemeral scratch memory (the Agent Scratchpad) and intent caching (procedural memory consolidation). It also asked for tool composition (the pipeline engine exists) and analytics (the audit log captures everything).

The agent didn't know about its own capabilities. This means the most impactful improvement isn't building new features — it's making existing features discoverable at the right moment.

The `agent-manual` tool exists but is pull-based: the agent must know to ask about a specific section. There's no mechanism for the system to say "hey, you have a tool for that" when the agent is about to reinvent the wheel.

---

## Current State

| Component | What Exists | Limitation |
|-----------|-------------|------------|
| `agent-manual` tool | 14 sections: index, tools, tool-detail, permissions, memory, events, commands, errors, feedback, agents, tasks, procedural, escalation, coordination | Agent must know what to ask for; no semantic search; no proactive suggestions |
| Tool manifests | Name, description, schema, permissions | Description is static; no semantic tags or capability categories |
| System prompt | Lists available tools by name | Too many tools (50+) for an LLM to hold in working memory; no grouping by intent |
| `agent-self` tool | Reports agent's name, ID, and tool list | Just a flat list of names — no descriptions, no "you could use X for Y" |

---

## Target Architecture

```
Agent says: "I need to save some intermediate results for later"
│
├── ContextInjector notices intent pattern matches "working memory" capability
│   └── Injects hint: "Available: scratch-write (scratchpad), memory-block-write (key-value)"
│
├── Agent uses scratch-write instead of reinventing storage
│   └── Tokens saved: ~2000 (avoided tool-exploration loop)
│
Alternative (pull-based):
├── Agent calls: agent-manual { section: "suggest", query: "save intermediate results" }
│   └── Returns ranked tool suggestions with descriptions and examples
```

---

## Phase Overview

| Phase | Name | Effort | Dependencies | Detail Doc | Status |
|-------|------|--------|-------------|------------|--------|
| 1 | Semantic tool search in agent-manual | 1d | None | [[01-semantic-tool-search]] | complete |
| 2 | Capability tags and tool grouping | 0.5d | Phase 1 | [[02-capability-tags-and-grouping]] | partial |
| 3 | Proactive capability hints in context injector | 0.5d | Phase 1, 2 | [[03-proactive-capability-hints]] | planned |

---

## Phase Dependency Graph

```mermaid
graph LR
    P1[Phase 1: Semantic tool search] --> P2[Phase 2: Capability tags]
    P1 --> P3[Phase 3: Proactive hints]
    P2 --> P3
```

---

## Key Design Decisions

1. **Extend `agent-manual`, don't create a new tool** — The manual already exists and agents know about it. Adding a `suggest` section that accepts a free-text query is the lowest-friction path. No new tool to register, no new manifest, no new permission.

2. **Embedding-based search over tool descriptions** — Use the existing `fastembed` MiniLM-L6-v2 embedder (already in `agentos-memory`) to embed tool descriptions + capability tags at boot. Query-time: embed the agent's query, cosine-similarity rank, return top 5 matches. This is the same infra the semantic memory uses.

3. **Capability tags in manifests, not code** — Add an optional `capability_tags` field to tool TOML manifests (e.g., `capability_tags = ["storage", "working-memory", "temporary-data"]`). Tags are free-text, embedded alongside the description. No ontology to maintain.

4. **Proactive hints are opt-in via kernel config** — `[tools] proactive_discovery = true|false`. When enabled, the context injector scans the last assistant message for intent patterns and injects a one-line hint if a relevant tool is found. Default: `false` (conservative).

5. **Hints are lightweight** — Max 1 hint per inference iteration, max 100 characters. This is a nudge, not a manual page. The agent can call `agent-manual suggest` for full details.

---

## Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Proactive hints add noise to context | Medium — wastes tokens if suggestions are wrong | Default off; max 1 per iteration; only trigger on high-confidence matches (cosine > 0.7) |
| Embedding boot cost | Low — 50 tools × 384-dim is ~20KB | Pre-compute at kernel boot; cache in memory; <100ms total |
| Agents over-rely on suggestions | Low — agents already reason about tools | Hints are suggestions, not commands; phrased as "Available: X" not "Use X" |
| Tag sprawl in manifests | Low — cosmetic | Tags are for search, not display; no UI or schema validation beyond basic types |

---

## Related

- [[Agent Scratchpad Plan]] — the feature the agent didn't know it had
- [[Multi-Agent Coordination Plan]] — coordination section of agent-manual already documents spawn/await
- [[Architecture Overview]]
