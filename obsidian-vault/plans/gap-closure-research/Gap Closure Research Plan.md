---
title: Gap Closure Research Plan
tags:
  - kernel
  - pipeline
  - tools
  - web
  - v3
  - plan
date: 2026-04-03
status: in-progress
effort: 2d
priority: high
---

# Gap Closure — Research-Identified Gaps

> Address 5 gaps identified by comparing AgentOS against current industry AI agent challenges via NotebookLM research.

---

## Problem

NotebookLM research (2026-04-03) comparing AgentOS to LangGraph, CrewAI, AutoGen, and PydanticAI identified gaps in:
1. Cost forecasting (no budget exhaustion projection)
2. Critic/verifier agent pattern (no built-in output verification)
3. Parallel pipeline execution (topological sort exists but execution is sequential)
4. Observability UI (SSE task streaming exists for dashboard but not for individual tasks)
5. Task trace visualization (SQLite trace data exists but no dedicated web endpoint)

## Phase Overview

| Phase | Name | Effort | Dependencies | Status |
|-------|------|--------|-------------|--------|
| 1 | Cost forecasting | 2h | None | planned |
| 2 | Verify-output tool | 1h | None | planned |
| 3 | Parallel pipeline execution | 3h | None | planned |
| 4 | SSE cost streaming | 1h | Phase 1 | planned |
| 5 | Task trace UI endpoint | 1h | None | planned |

## Key Design Decisions

1. Cost forecasting uses linear extrapolation from period_start to now → budget exhaustion ETA
2. Verify-output tool uses `_kernel_action` pattern (same as spawn-agent) to route to a verifier agent
3. Parallel pipeline uses `tokio::JoinSet` to run independent steps concurrently
4. SSE cost stream reuses existing events.rs pattern with cost-specific data
5. Task trace endpoint returns structured JSON from existing trace_collector SQLite

## Risks

| Risk | Mitigation |
|------|------------|
| Parallel pipeline changes step output ordering | Steps still wait for dependencies; only independent steps run concurrently |
| Cost forecast inaccurate for bursty workloads | Label it as "linear projection" in UI; add last-inference-rate as alternative |
