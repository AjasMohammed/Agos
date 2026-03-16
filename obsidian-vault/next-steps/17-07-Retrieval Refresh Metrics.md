---
title: Retrieval Refresh Metrics
tags:
  - memory
  - kernel
  - observability
  - next-steps
date: 2026-03-13
status: complete
effort: 2h
priority: high
---

# Retrieval Refresh Metrics

> Add metrics for retrieval refresh vs reuse decisions so runtime gains are measurable in production.

## What to Do

1. Add counters for retrieval refresh and retrieval reuse decisions.
2. Add a latency histogram for retrieval refresh duration.
3. Emit retrieval decision metrics in the task iteration loop.
4. Emit refresh duration and knowledge block count when refresh executes.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/metrics.rs` | New retrieval refresh metrics helpers |
| `crates/agentos-kernel/src/task_executor.rs` | Emit refresh/reuse counters and refresh latency |
| `crates/agentos-bus/src/message.rs` | Add `GetRetrievalMetrics` kernel command |
| `crates/agentos-kernel/src/commands/cost.rs` | Add retrieval metrics snapshot command handler |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch retrieval metrics command |
| `crates/agentos-cli/src/commands/cost.rs` | Add `cost retrieval` output command |

## Prerequisites

[[17-06-Memory Runtime Efficiency Hardening]]

## Verification

`cargo test -p agentos-kernel retrieval_gate`  
`cargo test -p agentos-kernel`  
`cargo test -p agentos-cli`  
`cargo build --workspace`
