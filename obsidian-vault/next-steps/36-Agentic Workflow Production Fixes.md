---
title: Agentic Workflow Production Fixes
tags:
  - kernel
  - tools
  - agent-experience
  - production
  - v3
  - next-steps
date: 2026-03-22
status: in-progress
effort: 2d
priority: critical
---

# Agentic Workflow Production Fixes

> Fix 10 issues discovered via deep codebase research that block pure agentic workflows: restrictive default permissions, missing intent types, silent execution failures, context loss on escalation, and inadequate system prompt.

---

## Current State

Deep research revealed agents are blocked from basic operations (file write, episodic write, messaging, delegation) by overly restrictive defaults. The task executor silently swallows errors, and escalations destroy context.

## Sub-tasks

| # | Task | Status |
|---|------|--------|
| P1-P7 | Expand default agent permissions | planned |
| A1-A2 | Add Delegate/Broadcast intents to task tokens | planned |
| T1 | Handle no-tool-call LLM responses gracefully | planned |
| T2 | Handle context fetch errors properly | planned |
| T3 | Handle tool result push failures | planned |
| T4 | Preserve context on escalation | planned |
| T6 | Add truncation awareness to tool output | planned |
| A3 | Declare required_permissions for scope-aware tools | planned |
| SP | Enrich system prompt with failure mode docs | planned |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
