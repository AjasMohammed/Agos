---
title: Memory Runtime Efficiency Hardening
tags:
  - memory
  - kernel
  - performance
  - next-steps
date: 2026-03-13
status: complete
effort: 6h
priority: high
---

# Memory Runtime Efficiency Hardening

> Reduce memory overhead and retrieval churn by sharing embedder instances and refreshing retrieval only when memory changes.

## What to Do

1. Create one shared `Embedder` in kernel boot and inject it into semantic/procedural stores.
2. Reuse kernel memory stores for tool runner registration instead of creating an additional memory stack.
3. Add retrieval dirty-flag logic in task execution to avoid re-running retrieval when memory state has not changed.
4. Mark retrieval context dirty on explicit memory mutations (`memory-write`, `archival-insert`, memory block write/delete kernel actions).
5. Keep correctness guarantees: first iteration always refreshes retrieval and memory blocks.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/kernel.rs` | Shared embedder + shared store wiring into tool runner |
| `crates/agentos-tools/src/runner.rs` | Add constructor accepting shared semantic/episodic stores |
| `crates/agentos-kernel/src/task_executor.rs` | Dirty-flag retrieval refresh logic |

## Prerequisites

[[17-01-Kernel Memory Wiring]], [[17-05-Context Freshness and Procedural Min Score]]

## Verification

`cargo test -p agentos-tools`  
`cargo test -p agentos-kernel retrieval_gate`  
`cargo test -p agentos-kernel`  
`cargo build --workspace`
