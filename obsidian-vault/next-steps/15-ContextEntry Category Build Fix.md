---
title: ContextEntry Category Build Fix
tags:
  - llm
  - v3
  - next-steps
date: 2026-03-12
status: complete
effort: 30m
priority: high
---

# ContextEntry Category Build Fix

> Unblock `cargo test --workspace` by backfilling the new `ContextEntry.category` field in LLM adapter tests.

---

## Current State

`ContextEntry` in `agentos-types` requires a `category` field, but some test initializers in `agentos-llm` were still constructing entries without it. A follow-up workspace run also surfaced additional stale test/config fixtures (`KernelConfig.context_budget` and a Gemini expectation mismatch).

## Goal / Target State

All stale fixtures compile with current types and behavior, and `cargo test --workspace` passes.

## Step-by-Step Plan

1. Add `category: ContextCategory::History` to missing `ContextEntry` test initializers in:
   - `crates/agentos-llm/src/openai.rs`
   - `crates/agentos-llm/src/ollama.rs`
2. Add missing `context_budget` in `KernelConfig` test config at:
   - `crates/agentos-cli/tests/common.rs`
3. Fix stale assertion values in:
   - `crates/agentos-llm/src/gemini.rs`
4. Run `cargo test --workspace` and confirm all tests pass.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/openai.rs` | Add missing `category` field in test context initializer |
| `crates/agentos-llm/src/ollama.rs` | Add missing `category` field in test context initializer |
| `crates/agentos-cli/tests/common.rs` | Add missing `context_budget` field to `KernelConfig` test fixture |
| `crates/agentos-llm/src/gemini.rs` | Align expected test output strings with test fixture contents |

## Verification

```bash
cargo test --workspace
```

## Related

- [[agentos-contextentry-category-backfill]]
- [[ContextEntry Category Backfill Flow]]
- [[Context Entry Categories]]
