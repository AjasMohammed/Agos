---
title: Consolidation and Memory Blocks
tags:
  - memory
  - consolidation
  - kernel
  - next-steps
date: 2026-03-13
status: complete
effort: 1d
priority: high
---

# Consolidation and Memory Blocks

> Add periodic episodic-to-procedural consolidation and per-agent memory block CRUD.

## What to Do

1. Add `consolidation.rs` with background cycle and pattern distillation.
2. Add `memory_blocks.rs` and kernel integration.
3. Add context injection of memory blocks.
4. Add core tool manifests and handlers for memory-block and archival operations.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/consolidation.rs` | New module |
| `crates/agentos-kernel/src/memory_blocks.rs` | New module |
| `crates/agentos-kernel/src/context.rs` | Memory block injection |
| `crates/agentos-kernel/src/kernel.rs` | Engine/store fields and init |
| `crates/agentos-tools/src/*` | New tool handlers and runner wiring |
| `tools/core/*.toml` | New memory-block and archival manifests |

## Prerequisites

[[17-01-Kernel Memory Wiring]], [[17-02-Adaptive Retrieval Gate Implementation]]

## Verification

`cargo test -p agentos-kernel`  
`cargo test -p agentos-tools`  
`cargo build --workspace`
