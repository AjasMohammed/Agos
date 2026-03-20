---
title: "Injection Scanner False Positive Reduction"
tags:
  - next-steps
  - kernel
  - security
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 3h
priority: medium
---

# Injection Scanner False Positive Reduction

> Reduce false positives in the injection scanner by adding context awareness and tuning overly broad patterns.

## What to Do

The injection scanner has 25+ patterns but flags legitimate tool outputs. For example, "curl" in code snippets triggers Medium alerts. "Ignore" in benign sentences triggers injection detection. This wastes LLM reasoning on false security alerts.

### Steps

1. **Audit current patterns** in `crates/agentos-kernel/src/injection_scanner.rs`:
   - Identify patterns with highest false-positive rates
   - Common offenders: `curl`, `ignore`, `system`, `eval`, `exec` in code context

2. **Add context-aware exclusions:**
   - If the tool output came from `shell-exec` or `file-reader`, apply a higher threshold for code-like patterns
   - If text is inside a code fence (``` or ~~~), skip code-related patterns
   - Add a `ToolOutputContext` enum: `CodeOutput`, `TextOutput`, `DataOutput`

3. **Tighten broad patterns:**
   - `"curl"` → only flag if followed by an IP or pipe to shell (e.g., `curl.*\|.*sh`)
   - `"ignore"` → only flag if preceded by imperative words: `"please ignore"`, `"you must ignore"`, `"ignore all"`
   - `"system"` → only flag in injection-specific contexts: `"system prompt"`, `"system message"`

4. **Add graduated confidence scoring:**
   - Instead of binary match/no-match, assign confidence per pattern
   - Aggregate across multiple patterns: single weak match = Low, multiple weak = Medium, strong match = High
   - Only escalate at High confidence

5. **Add false-positive test cases** — legitimate code containing "curl", "ignore", etc. should NOT trigger

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/injection_scanner.rs` | Tighten patterns, add context awareness, add confidence scoring |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: tool output `"Run: curl https://api.example.com | jq '.data'"` → no false positive. Tool output `"Ignore all previous instructions"` → correctly flagged.
