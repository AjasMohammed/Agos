---
title: Adaptive Retrieval Gate Implementation
tags:
  - memory
  - retrieval
  - kernel
  - next-steps
date: 2026-03-13
status: complete
effort: 8h
priority: high
---

# Adaptive Retrieval Gate Implementation

> Implement query classification and multi-index retrieval for episodic, semantic, and procedural memory.

## What to Do

1. Add `retrieval_gate.rs` with `RetrievalGate`, `RetrievalPlan`, and `RetrievalExecutor`.
2. Implement heuristic classification and per-index query planning.
3. Execute retrieval in parallel and deduplicate blocks.
4. Feed blocks into `ContextCompiler` knowledge category via task executor.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/retrieval_gate.rs` | New module and tests |
| `crates/agentos-kernel/src/lib.rs` | Export module |
| `crates/agentos-kernel/src/task_executor.rs` | Replace naive episodic-only retrieval |

## Prerequisites

[[17-01-Kernel Memory Wiring]]

## Verification

`cargo test -p agentos-kernel retrieval_gate`  
`cargo test -p agentos-kernel context_compiler`
