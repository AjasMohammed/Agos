---
title: Structured Memory Extraction Engine
tags:
  - memory
  - extraction
  - kernel
  - next-steps
date: 2026-03-13
status: complete
effort: 8h
priority: high
---

# Structured Memory Extraction Engine

> Add typed tool-output extraction and semantic-memory writes without extra LLM calls.

## What to Do

1. Add `memory_extraction.rs` with extractor trait and default extractor registry.
2. Implement extraction for core tool outputs and conflict detection against semantic memory.
3. Wire extraction engine into kernel boot and tool-result handling.
4. Add config wiring under `[memory.extraction]`.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/memory_extraction.rs` | New module and tests |
| `crates/agentos-kernel/src/kernel.rs` | Engine initialization |
| `crates/agentos-kernel/src/task_executor.rs` | Async extraction trigger |
| `crates/agentos-kernel/src/config.rs` | Extraction config |
| `config/default.toml` | Extraction defaults |

## Prerequisites

[[17-01-Kernel Memory Wiring]]

## Verification

`cargo test -p agentos-kernel memory_extraction`  
`cargo build --workspace`
