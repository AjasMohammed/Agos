---
title: Kernel Memory Wiring
tags:
  - memory
  - kernel
  - next-steps
date: 2026-03-13
status: complete
effort: 6h
priority: critical
---

# Kernel Memory Wiring

> Wire semantic and procedural stores into `Kernel` and use them in task execution flow.

## What to Do

1. Add `semantic_memory` and `procedural_memory` fields to `Kernel`.
2. Initialize both stores in `Kernel::boot()` using `model_cache_dir`.
3. Keep existing `ContextCompiler` flow and ensure retrieval hooks can access both stores.
4. Add kernel-level tests/build checks for boot path compatibility.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/kernel.rs` | Add fields and boot wiring |
| `crates/agentos-kernel/src/task_executor.rs` | Use wired stores in retrieval path |

## Prerequisites

None

## Verification

`cargo build -p agentos-kernel`  
`cargo test -p agentos-kernel`
