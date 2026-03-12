---
title: Context Freshness and Procedural Min Score
tags:
  - memory
  - kernel
  - procedural
  - next-steps
date: 2026-03-13
status: in-progress
effort: 4h
priority: high
---

# Context Freshness and Procedural Min Score

> Fix stale per-iteration memory context and restore procedural search minimum score filtering.

## What to Do

1. Move adaptive retrieval execution and memory block injection inside the task iteration loop in `task_executor.rs`.
2. Keep retrieval classification stable, but refresh retrieved knowledge and memory blocks on each iteration before context compilation.
3. Update `ProceduralStore::search()` to accept `min_score: f32`.
4. Validate `min_score` range (0.0 to 1.0) and filter low-similarity results.
5. Update procedural search call sites to pass explicit `min_score`.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_executor.rs` | Refresh retrieval/memory block knowledge per iteration |
| `crates/agentos-memory/src/procedural.rs` | Add `min_score` arg + validation/filter |
| `crates/agentos-kernel/src/retrieval_gate.rs` | Pass `min_score` in procedural retrieval path |
| `crates/agentos-kernel/src/consolidation.rs` | Pass `min_score` when deduping procedures |

## Prerequisites

[[17-01-Kernel Memory Wiring]], [[17-02-Adaptive Retrieval Gate Implementation]], [[17-04-Consolidation and Memory Blocks]]

## Verification

`cargo test -p agentos-memory procedural`  
`cargo test -p agentos-kernel retrieval_gate`  
`cargo test -p agentos-kernel task_executor`  
`cargo build --workspace`
