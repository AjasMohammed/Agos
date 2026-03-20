---
title: "Add Input Schemas to All TOML Manifests"
tags:
  - next-steps
  - tools
  - manifests
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 1d
priority: critical
---

# Add Input Schemas to All TOML Manifests

> Add `[input_schema]` JSON Schema definitions to all 40+ tool manifests so agents can construct correct payloads without trial-and-error.

## What to Do

All 40+ TOML manifests in `tools/core/` lack `input_schema` fields. As an LLM constructing tool payloads as JSON, the agent must guess each tool's expected fields. The `agent-manual` tool-detail section shows `null` for input_schema.

### Steps

1. **Define the TOML schema format** — add `[input_schema]` as a table with JSON Schema properties:
   ```toml
   [input_schema]
   type = "object"
   required = ["path"]

   [input_schema.properties.path]
   type = "string"
   description = "File path relative to data directory"

   [input_schema.properties.offset]
   type = "integer"
   description = "Line offset to start reading from"
   default = 0
   ```

2. **Update `ToolManifest` struct** in `agentos-types/src/tool.rs`:
   - Add `input_schema: Option<serde_json::Value>` field
   - Parse from TOML during tool loading

3. **Add schemas to each manifest** — review each tool's `execute()` method to determine the expected input fields. Priority order:
   - **File tools:** file-reader, file-writer, file-editor, file-glob, file-grep, file-delete, file-move, file-diff
   - **Memory tools:** memory-write, memory-search, memory-read, memory-delete, memory-stats, episodic-list
   - **Procedure tools:** procedure-create, procedure-delete, procedure-list, procedure-search
   - **Network tools:** http-client, web-fetch
   - **Agent tools:** agent-manual, agent-list, agent-message
   - **Execution tools:** shell-exec, data-parser
   - **Utility tools:** think, datetime, task-list, task-status

4. **Wire into `agent-manual` tool-detail section** in `crates/agentos-tools/src/agent_manual.rs`:
   - When rendering tool detail, include the input_schema from the manifest
   - Format as readable JSON Schema documentation

5. **Wire into `tools_for_prompt()`** in `crates/agentos-kernel/src/tool_registry.rs`:
   - Include a compact schema summary in the system prompt tool listing

## Files Changed

| File | Change |
|------|--------|
| `tools/core/*.toml` (40+ files) | Add `[input_schema]` table |
| `crates/agentos-tools/src/traits.rs` | Add `input_schema` to `ToolManifest` |
| `crates/agentos-tools/src/agent_manual.rs` | Display schema in tool-detail |
| `crates/agentos-kernel/src/tool_registry.rs` | Include schema in `tools_for_prompt()` |

## Prerequisites

None — can be done in parallel with other Phase 1 tasks.

## Verification

```bash
cargo test -p agentos-tools
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: `agent-manual` tool-detail for "file-reader" shows the complete input schema with field names, types, and descriptions. `tools_for_prompt()` output includes schema info.
