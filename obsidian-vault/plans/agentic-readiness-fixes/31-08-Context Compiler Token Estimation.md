---
title: "Context Compiler Token Estimation and Budget Query"
tags:
  - next-steps
  - kernel
  - context
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 4h
priority: high
---

# Context Compiler Token Estimation and Budget Query

> Replace the `chars / 4` token estimation heuristic with a configurable ratio and add a method to query remaining budget per category.

## What to Do

The context compiler uses `chars().count() / 4 + 1` for token estimation. This can be 30%+ off for non-Latin text (Chinese/Japanese: 1 char ≈ 1-2 tokens). Additionally, there's no way for an agent to ask "how many tokens do I have left for Knowledge?"

### Steps

1. **Make token estimation ratio configurable** in `crates/agentos-kernel/src/context_compiler.rs`:
   - Add `chars_per_token: f32` config field (default: 4.0)
   - Replace hardcoded `/ 4` with `/ chars_per_token as usize`
   - Add to `config/default.toml`:
     ```toml
     [kernel.context]
     chars_per_token = 4.0
     ```

2. **Add `estimated_tokens_remaining(category)` method** to `ContextWindow` in `crates/agentos-types/src/context.rs`:
   - Calculate: `budget_for_category - sum(estimated_tokens for entries in category)`
   - Return as `usize`
   - Add `remaining_budget_summary()` → `HashMap<ContextCategory, usize>` for full overview

3. **Deduplicate context entries** in the compiler:
   - Before inserting an entry, check if an entry with identical content already exists
   - If duplicate found, skip insertion
   - Use a lightweight hash (not cryptographic) for fast comparison

4. **Fix summary entry accumulation:**
   - Entries created by `Summarize` overflow strategy get `role: System`
   - These are never evicted since system entries are pinned in FIFO
   - Add a `is_summary: bool` field to `ContextEntry` or use a dedicated `ContextCategory::Summary`
   - Allow summary entries to be evicted when space is needed

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/context_compiler.rs` | Configurable ratio, dedup |
| `crates/agentos-types/src/context.rs` | Add `estimated_tokens_remaining()`, fix summary accumulation |
| `config/default.toml` | Add `chars_per_token` |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-kernel
cargo test -p agentos-types
cargo clippy --workspace -- -D warnings
```
