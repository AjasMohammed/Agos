---
title: "tools_for_prompt() with Schemas and Permissions"
tags:
  - next-steps
  - kernel
  - tools
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 3h
priority: high
---

# tools_for_prompt() with Schemas and Permissions

> Enhance `tools_for_prompt()` to include input schemas and required permissions so the system prompt tells agents how to call tools, not just that they exist.

## What to Do

Currently `tools_for_prompt()` in `tool_registry.rs` returns `"- name : description"` — no input schema, no permissions. An agent sees tools but doesn't know how to call them without consulting agent-manual each time.

### Steps

1. **Enhance `tools_for_prompt()` output format** in `crates/agentos-kernel/src/tool_registry.rs`:
   ```
   ## file-reader
   Read files from the data directory
   Permissions: fs.user_data:r
   Input: { "path": string (required), "offset": integer, "limit": integer }

   ## memory-search
   Search semantic memory by query
   Permissions: memory:r
   Input: { "query": string (required), "top_k": integer, "min_score": float }
   ```

2. **Read `input_schema` from manifests** (depends on [[31-03-Input Schemas for TOML Manifests]] adding schemas):
   - If `input_schema` present: render a compact one-line JSON schema summary
   - If absent: show `Input: (see agent-manual tool-detail)`

3. **Include required permissions** from manifest `capabilities` section

4. **Add `search_by_capability()` method** to tool registry:
   - Accept a capability string like `"fs"`, `"memory"`, `"net"`
   - Return all tools whose permissions include that capability prefix
   - Useful for agents asking "which tools can write files?"

5. **Update agent-manual `commands` section** to distinguish tool-accessible vs kernel-only commands

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/tool_registry.rs` | Enhance `tools_for_prompt()`, add `search_by_capability()` |
| `crates/agentos-tools/src/agent_manual.rs` | Update commands section distinction |

## Prerequisites

- [[31-03-Input Schemas for TOML Manifests]] (for full schema display; works without it in degraded mode)

## Verification

```bash
cargo test -p agentos-kernel
cargo test -p agentos-tools
cargo clippy --workspace -- -D warnings
```

Test: `tools_for_prompt()` output includes permission strings and schema summary for tools that have schemas.
