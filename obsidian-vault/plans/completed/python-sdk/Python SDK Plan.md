---
title: AgentOS Python SDK
tags:
  - sdk
  - tools
  - python
  - runtime
date: 2026-04-14
status: complete
effort: 3d
priority: high
---

# AgentOS Python SDK

> A Python package (`agentos-sdk`) that lets users write AgentOS tools as decorated functions. The SDK generates a JSON Schema from type annotations, handles all I/O boilerplate, and installs the tool into the running kernel via `agentos tool install <file.py>` — one command, no manifest, no restart.

---

## Architecture

```
User writes stock_price.py with @tool decorator
    ↓
agentos tool install stock_price.py
    ↓ (Rust CLI)
runs: python3 stock_price.py --agentos-manifest
    ↓ (SDK prints JSON manifest)
Rust generates wrapper script in $data_dir/scripts/stock-price.py
    ↓ (ScriptWatcher inotify fires)
ScriptParser reads @schema: annotation
    ↓
ToolRunner.register_dynamic_with_schema(ScriptTool, schema)
    ↓
Agent sees tool with full JSON Schema in system prompt
```

---

## Phase Overview

| Phase | Name | Effort | Detail Doc | Status |
|---|---|---|---|---|
| 1 | Python SDK core | 1d | [[01-sdk-core]] | complete |
| 2 | CLI integration (.py install) | 0.5d | [[02-cli-integration]] | complete |
| 3 | Schema surfacing in kernel | 0.5d | [[03-schema-surfacing]] | complete |

---

## Key Design Decisions

1. **Zero external dependencies** — SDK core uses only stdlib. No pydantic required, though Pydantic models are supported if installed.
2. **`--agentos-manifest` flag** — the SDK file is self-describing. Running `python file.py --agentos-manifest` prints JSON manifest. This is how the CLI extracts metadata without importing user code in-process.
3. **Wrapper script pattern** — `agentos tool install` generates a wrapper in `$data_dir/scripts/`. The original file is never modified.
4. **`@schema:` annotation** — the wrapper carries the full JSON Schema as a single comment annotation. ScriptParser reads it and passes it to ToolRunner.
5. **`fn_name` in manifest** — the decorator stores the original function name so the wrapper can import it correctly.
