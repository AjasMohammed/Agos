---
title: "Configurable Anthropic Max Tokens"
tags:
  - next-steps
  - llm
  - anthropic
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 1h
priority: high
---

# Configurable Anthropic Max Tokens

> Make the `max_tokens` parameter configurable instead of hardcoded to 4096, and fix the Ollama context window hardcoding.

## What to Do

The Anthropic adapter hardcodes `max_tokens: 4096`. Long-form generation is silently truncated. The Ollama adapter hardcodes context window at 8,192 tokens when many models support 32K+.

### Steps

1. **Add LLM config fields** to `config/default.toml`:
   ```toml
   [llm]
   max_tokens = 8192            # Default max output tokens
   ollama_context_window = 32768  # Ollama context size
   ```

2. **Update Anthropic adapter** in `crates/agentos-llm/src/anthropic.rs`:
   - Read `max_tokens` from config instead of hardcoding 4096
   - Allow per-request override if `ContextWindow` has a max_tokens field

3. **Update Ollama adapter** in `crates/agentos-llm/src/ollama.rs`:
   - Read context window size from config instead of hardcoding 8192

4. **Add to `LLMCapabilities`** — expose `max_output_tokens` so callers can check the limit

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-llm/src/anthropic.rs` | Read `max_tokens` from config |
| `crates/agentos-llm/src/ollama.rs` | Read context window from config |
| `config/default.toml` | Add LLM config fields |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-llm
cargo clippy --workspace -- -D warnings
```
