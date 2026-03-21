---
title: Sandbox Lightweight Execution
tags:
  - kernel
  - sandbox
  - security
  - v3
  - next-steps
date: 2026-03-21
status: planned
effort: 10d
priority: critical
---

# Sandbox Lightweight Execution

> Eliminate ~1 GiB virtual memory overhead per sandbox child by replacing full ToolRunner construction with a single-tool lazy factory, making tool manifest resource declarations meaningful.

---

## Current State

Sandbox children re-exec the full `agentctl` binary, construct all 35+ tools, load the fastembed ML model (~23 MB), and open 3 SQLite databases -- even for zero-dependency tools like `datetime` (declared 4 MB / 100 ms). RLIMIT_AS has a 1 GiB floor that masks this waste.

## Goal / Target State

Sandbox children construct only the single requested tool and its actual dependencies. Stateless tools use ~128 MB VM. Memory tools use ~768 MB. Manifest `max_memory_mb` becomes the dominant factor in RLIMIT_AS, not process overhead.

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[01-single-tool-factory]] | `crates/agentos-tools/src/factory.rs`, `lib.rs` | planned |
| 02 | [[02-sandbox-child-lazy-init]] | `crates/agentos-cli/src/main.rs` | planned |
| 03 | [[03-per-category-rlimit]] | `crates/agentos-sandbox/src/executor.rs`, `config.rs`, `task_executor.rs` | planned |
| 04 | [[04-tool-weight-classification]] | `crates/agentos-types/src/tool.rs`, `factory.rs`, `task_executor.rs` | planned |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings

# Manual: run datetime tool in sandbox, verify rlimit_as_mb ~132 (not ~1024)
```

## Related

[[Sandbox Lightweight Execution Plan]]
