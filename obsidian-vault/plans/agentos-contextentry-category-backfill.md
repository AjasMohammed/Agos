---
title: ContextEntry Category Backfill Design
tags:
  - llm
  - v3
  - plan
date: 2026-03-12
status: complete
effort: 20m
priority: high
---

# ContextEntry Category Backfill Design

> Use an explicit category backfill in LLM tests to keep type safety strict and restore workspace build health.

---

## Problem

`ContextEntry` gained a required `category` field. Older test fixtures in `agentos-llm` were not updated, so the workspace fails compilation with missing-field errors.

## Options Considered

1. Add a `Default`-based helper constructor and migrate tests gradually.
2. Make `category` optional in `ContextEntry`.
3. Backfill missing test initializers immediately with explicit `ContextCategory`.

## Decision

Use option 3 now: add explicit `category: ContextCategory::History` to missing initializers in failing test files. Keep `category` required to preserve compile-time invariants for production code.

## Consequences

- Immediate: workspace compile is unblocked.
- Positive: tests remain explicit and aligned with typed context budgeting semantics.
- Constraint: future `ContextEntry` additions will continue requiring fixture updates.

## Related

- [[15-ContextEntry Category Build Fix]]
- [[ContextEntry Category Backfill Flow]]
