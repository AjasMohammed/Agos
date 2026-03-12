---
title: Context Entry Categories
tags:
  - types
  - llm
  - reference
date: 2026-03-12
status: partial
effort: 20m
priority: medium
---

# Context Entry Categories

> `ContextCategory` labels each `ContextEntry` for budgeted context compilation and deterministic prompt assembly.

---

## Overview

`ContextCategory` is defined in `crates/agentos-types/src/context.rs` and attached to every `ContextEntry`. It is consumed by context compilation and token budget allocation logic.

## Configuration

Category budget percentages are defined by `TokenBudget` in `crates/agentos-types/src/context.rs`:

- `system_pct`
- `tools_pct`
- `knowledge_pct`
- `history_pct`
- `task_pct`

## API / CLI

No direct CLI commands currently expose category assignment. Category values are set by code that constructs `ContextEntry` objects.

## Internals

`ContextCategory` variants:

- `System`
- `Tools`
- `Knowledge`
- `History` (default)
- `Task`

`ContextEntry` requires `category` and uses `#[serde(default)]` for deserialization compatibility, while direct struct initialization still must provide a value.

## Related

- [[15-ContextEntry Category Build Fix]]
- [[agentos-contextentry-category-backfill]]
- [[ContextEntry Category Backfill Flow]]
