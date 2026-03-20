---
title: Add Tool Manifest and Web UI Link
tags:
  - tools
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 1h
priority: medium
---

# Add Tool Manifest and Web UI Link

> Create the `agent-manual.toml` manifest in `tools/core/` so the tool appears in the registry, and add a "Manual" link in the Web UI sidebar.

---

## Why This Subtask

The tool implementation and registration are complete (subtasks 01-03), but the tool needs a TOML manifest in `tools/core/` so that:
1. It appears in `ToolRegistry::list_all()` and `tools_for_prompt()`.
2. It has an official description, version, and permission declaration.
3. The Web UI can link to it or embed a manual viewer.

The Web UI integration is lightweight -- just a link in the sidebar navigation.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `tools/core/agent-manual.toml` | Does not exist | New manifest file |
| Web UI sidebar | No "Manual" link | "Manual" link added (links to `/manual` or shows inline manual section) |

---

## What to Do

### 1. Create `tools/core/agent-manual.toml`

```toml
[manifest]
name        = "agent-manual"
version     = "1.0.0"
description = "Query structured AgentOS documentation: tools, permissions, memory, events, commands, errors. Use {\"section\": \"index\"} to see all sections."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = []

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "ManualQuery"
output = "ManualSection"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

Key points:
- `permissions = []` -- no permissions required.
- `max_memory_mb = 16` -- the tool returns static JSON, very lightweight.
- `max_cpu_ms = 1000` -- responses are instantaneous.

### 2. Add input schema (optional but recommended)

Add an `input_schema` section to the manifest to enable JSON Schema validation:

```toml
[input_schema]
type = "object"
required = ["section"]

[input_schema.properties.section]
type = "string"
enum = ["index", "tools", "tool-detail", "permissions", "memory", "events", "commands", "errors", "feedback"]
description = "Which manual section to query"

[input_schema.properties.name]
type = "string"
description = "Tool name (required for tool-detail section)"
```

**Note:** TOML nested tables for JSON Schema can be awkward. If the schema registry does not support inline TOML schemas, provide it as inline JSON instead:

```toml
input_schema = """
{
  "type": "object",
  "required": ["section"],
  "properties": {
    "section": {
      "type": "string",
      "enum": ["index", "tools", "tool-detail", "permissions", "memory", "events", "commands", "errors", "feedback"],
      "description": "Which manual section to query"
    },
    "name": {
      "type": "string",
      "description": "Tool name (required when section is tool-detail)"
    }
  }
}
"""
```

Check how other manifests handle `input_schema`. If none of the existing `.toml` manifests use `input_schema`, skip this and just use the base manifest above.

### 3. Add Web UI sidebar link

Open `crates/agentos-web/src/templates/base.html`. In the sidebar navigation section (look for the `<nav>` or sidebar `<div>` containing links like "Dashboard", "Tasks", "Agents", "Tools", etc.), add a new entry:

```html
<a href="/tools" class="sidebar-link">
    <span class="sidebar-icon">&#128214;</span>
    Manual
</a>
```

Or if the sidebar uses a different pattern (check the actual template), adapt accordingly. The link can point to `/tools` with a note, or to a dedicated `/manual` route.

**Simpler alternative:** If modifying the Web UI is out of scope for this plan, skip this step. The tool is already usable by agents via tool calls. The Web UI link is a nice-to-have for human operators.

---

## Files Changed

| File | Change |
|------|--------|
| `tools/core/agent-manual.toml` | New file: tool manifest |
| `crates/agentos-web/src/templates/base.html` | Add "Manual" link in sidebar (optional) |

---

## Prerequisites

[[27-03-Wire AgentManual into ToolRunner and Registry]] must be complete (the tool must be registered in the runner).

---

## Test Plan

- `cargo build --workspace` must compile.
- Verify the manifest parses correctly:

```rust
// In a test or manual verification:
let manifest = agentos_tools::loader::load_manifest(
    std::path::Path::new("tools/core/agent-manual.toml")
).unwrap();
assert_eq!(manifest.manifest.manifest.name, "agent-manual");
assert_eq!(manifest.manifest.manifest.trust_tier, agentos_types::TrustTier::Core);
assert!(manifest.manifest.capabilities_required.permissions.is_empty());
```

- After kernel boot, verify `agent-manual` appears in the tool list:

```bash
# If kernel is running:
agentctl tool list
# Should show agent-manual in the output
```

---

## Verification

```bash
# Verify manifest is valid TOML that parses as ToolManifest
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings

# Manual check: read the manifest
cat tools/core/agent-manual.toml
```
