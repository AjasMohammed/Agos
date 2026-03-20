---
title: 30-01 Missing Tool Manifests
tags:
  - tools
  - manifest
  - next-steps
  - subtask
date: 2026-03-18
status: planned
effort: 4h
priority: critical
---

# 30-01 — Missing Tool Manifests

> Write TOML manifests for 9 implemented tools that are invisible to agents because they have no file in `tools/core/`.

---

## Why This Phase

The tool runner registers 27+ tools at startup but only 20 have TOML manifests. An agent using `agent-manual` (section `tools`) only sees the 20 with manifests. The 9 hidden tools are fully functional — they just need a declaration file.

No Rust code changes required. Only new TOML files.

---

## Current → Target State

| Tool | Status |
|------|--------|
| `procedure-create` | implemented, **no manifest** |
| `procedure-search` | implemented, **no manifest** |
| `memory-delete` | implemented, **no manifest** |
| `memory-stats` | implemented, **no manifest** |
| `task-delegate` | implemented, **no manifest** |
| `process-manager` | implemented, **no manifest** |
| `log-reader` | implemented, **no manifest** |
| `network-monitor` | implemented, **no manifest** |
| `hardware-info` | implemented, **no manifest** |

---

## What to Do

**Before writing:** Read one existing manifest as a format reference:
```
tools/core/memory-write.toml
tools/core/shell-exec.toml
```

Format template:
```toml
[manifest]
name        = "<tool-name>"
version     = "1.0.0"
description = "<one-sentence description>"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["<resource>:<op>", ...]

[capabilities_provided]
outputs = ["<output-type>"]

[intent_schema]
input  = "<InputType>"
output = "<OutputType>"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = <N>
max_cpu_ms    = <N>
syscalls      = []
```

---

### 1. `tools/core/procedure-create.toml`

Source: `crates/agentos-tools/src/procedure_create.rs`

- Required permissions: `memory.procedural:w`
- Input fields: `name` (required), `description` (required), `steps` (array of `{action, tool?, expected_outcome?}`), `preconditions` (string array, optional), `postconditions` (string array, optional), `tags` (string array, optional)
- Output: `{success, id, name, message}`

```toml
[manifest]
name        = "procedure-create"
version     = "1.0.0"
description = "Record a step-by-step procedure in procedural memory for future reuse"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.procedural:w"]

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "ProcedureCreateIntent"
output = "ProcedureCreateResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 64
max_cpu_ms    = 5000
syscalls      = []
```

---

### 2. `tools/core/procedure-search.toml`

Source: `crates/agentos-tools/src/procedure_search.rs`

- Required permissions: `memory.procedural:r`
- Input fields: `query` (required string), `top_k` (optional u64, default 5, max 20), `min_score` (optional f64, default 0.0)
- Output: `{query, count, results: [{id, name, description, steps, tags, success_count, failure_count, semantic_score, rrf_score}]}`

```toml
[manifest]
name        = "procedure-search"
version     = "1.0.0"
description = "Search procedural memory for stored how-to procedures matching a query"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.procedural:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "ProcedureSearchIntent"
output = "ProcedureSearchResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 128
max_cpu_ms    = 8000
syscalls      = []
```

---

### 3. `tools/core/memory-delete.toml`

Source: `crates/agentos-tools/src/memory_delete.rs`

- Required permissions: `memory.semantic:w`
- Input fields: `id` (required string — the UUID of the semantic memory entry)
- Output: `{success, deleted_id, message}`

```toml
[manifest]
name        = "memory-delete"
version     = "1.0.0"
description = "Delete a specific semantic memory entry by ID to remove stale or incorrect knowledge"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.semantic:w"]

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "MemoryDeleteIntent"
output = "DeleteResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 2000
syscalls      = []
```

---

### 4. `tools/core/memory-stats.toml`

Source: `crates/agentos-tools/src/memory_stats.rs`

- Required permissions: `memory.semantic:r` (minimum; tool adapts to whatever read perms the agent has)
- Input: no required fields (payload ignored)
- Output: `{agent_id, tiers: {semantic: {entries, description}, episodic: {entries, description}, procedural: {entries, description}}, total_entries}`

```toml
[manifest]
name        = "memory-stats"
version     = "1.0.0"
description = "Return entry counts for all memory tiers (semantic, episodic, procedural) for the calling agent"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["memory.semantic:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "MemoryStatsQuery"
output = "MemoryStatsResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 3000
syscalls      = []
```

---

### 5. `tools/core/task-delegate.toml`

Source: `crates/agentos-tools/src/task_delegate.rs`

- Required permissions: `agent.message:x`
- Input fields: `agent` (required string — target agent name), `task` (required string — prompt for sub-agent), `priority` (optional u8, default 5)
- Output: `{_kernel_action: "delegate_task", target_agent, task, priority}` — kernel intercepts `_kernel_action`

```toml
[manifest]
name        = "task-delegate"
version     = "1.0.0"
description = "Delegate a sub-task to a named agent and return immediately (non-blocking)"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["agent.message:x"]

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "TaskDelegateIntent"
output = "DelegateResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

---

### 6. `tools/core/process-manager.toml`

Source: `crates/agentos-tools/src/process_manager.rs`

- Required permissions: `process.list:r` (for `action=list`), `process.kill:x` (for `action=kill`)
- Input fields: `action` (string, "list" or "kill"), `pid` (optional u32, required for kill)
- Output: HAL-provided process data

```toml
[manifest]
name        = "process-manager"
version     = "1.0.0"
description = "List running processes or terminate a process by PID (requires HAL)"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["process.list:r", "process.kill:x"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "ProcessManagerIntent"
output = "ProcessManagerResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 5000
syscalls      = []
```

---

### 7. `tools/core/log-reader.toml`

Source: `crates/agentos-tools/src/log_reader.rs`

- Required permissions: `fs.app_logs:r`, `fs.system_logs:r`
- Input: forwarded to `hal.query("log", payload, &perms)` — format determined by HAL
- Output: HAL-provided log lines

```toml
[manifest]
name        = "log-reader"
version     = "1.0.0"
description = "Read application or system log entries via the Hardware Abstraction Layer"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["fs.app_logs:r", "fs.system_logs:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "LogReaderIntent"
output = "LogReaderResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 64
max_cpu_ms    = 5000
syscalls      = []
```

---

### 8. `tools/core/network-monitor.toml`

Source: `crates/agentos-tools/src/network_monitor.rs`

- Required permissions: `network.logs:r`
- Input: forwarded to `hal.query("network", payload, &perms)`
- Output: HAL-provided network stats

```toml
[manifest]
name        = "network-monitor"
version     = "1.0.0"
description = "Query network interface statistics and connection logs via the Hardware Abstraction Layer"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["network.logs:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "NetworkMonitorIntent"
output = "NetworkMonitorResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 3000
syscalls      = []
```

---

### 9. `tools/core/hardware-info.toml`

Source: `crates/agentos-tools/src/hardware_info.rs`

- Required permissions: `hardware.system:r`
- Input: none (payload ignored, calls `hal.query("system", {}, &perms)`)
- Output: HAL-provided system info (CPU, RAM, GPU, disk)

```toml
[manifest]
name        = "hardware-info"
version     = "1.0.0"
description = "Return hardware capabilities: CPU, memory, GPU, and disk capacity via the Hardware Abstraction Layer"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["hardware.system:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "HardwareInfoQuery"
output = "HardwareInfoResult"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 2000
syscalls      = []
```

---

## Files Changed

| File | Change |
|------|--------|
| `tools/core/procedure-create.toml` | Create |
| `tools/core/procedure-search.toml` | Create |
| `tools/core/memory-delete.toml` | Create |
| `tools/core/memory-stats.toml` | Create |
| `tools/core/task-delegate.toml` | Create |
| `tools/core/process-manager.toml` | Create |
| `tools/core/log-reader.toml` | Create |
| `tools/core/network-monitor.toml` | Create |
| `tools/core/hardware-info.toml` | Create |

---

## Prerequisites

None — no Rust changes required.

## Verification

```bash
# Confirm manifests parse without error
cargo test -p agentos-tools -- manifest
# Confirm loader picks them up
cargo test -p agentos-kernel -- tool_registry
# Count visible tools (should be 29 after this phase)
ls tools/core/*.toml | wc -l
```
