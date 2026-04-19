---
title: Script Modules — Drop-in Polyglot Tool Authoring
tags:
  - kernel
  - tools
  - runtime
  - scripting
  - v4
date: 2026-04-14
status: complete
effort: 4d
priority: high
---

# Script Modules — Drop-in Polyglot Tool Authoring

> Users drop a script in any language into `$data_dir/scripts/`. The kernel discovers it instantly via inotify, reads annotation headers embedded in the script's own comments, generates a manifest in-memory, and registers it as a first-class tool — no TOML file, no `agentos tool install`, no compilation, no kernel restart.

---

## Why This Matters

Today, writing a custom tool for AgentOS requires:
1. Implementing the `AgentTool` trait in Rust
2. Writing a separate 15-field TOML manifest
3. Running `agentos tool install /full/path/to/manifest.toml`
4. Ensuring the kernel is restarted or `ToolLoad` is called manually

This is a **builder workflow** — it is not accessible to users who just want to automate something. The gap between "I have a script" and "the agent can use my script" is too wide.

Script Modules close this gap entirely. The filesystem becomes the package manager. A script is its own manifest. Writing = installing.

---

## Core Insight

AgentOS already has all the pieces:
- `bwrap` sandbox in `shell-exec` — arbitrary process isolation
- `register_dynamic()` + `dynamic_tools: RwLock<HashMap>` in `ToolRunner` — zero-restart registration
- `ConfigWatcher` using `notify` crate — inotify-driven file watching
- WASM `stdin → stdout` contract — stdin payload, stdout result
- `ToolManifest` already constructible in-memory — no file needed

Script Modules = wire these together with a comment-based annotation parser.

---

## Design: The Annotation Contract

The script IS the manifest. Annotations live in comment lines at the top of the file:

```bash
#!/bin/bash
# @agentos tool: weather-lookup
# @description: Fetches current weather for a city using wttr.in
# @permissions: network.outbound:x
# @risk: readonly_external
# @timeout: 15
# @version: 1.0.0
# @tags: weather, network

CITY=$(echo "$AGENTOS_INPUT" | jq -r '.city // "London"')
curl -s "https://wttr.in/${CITY}?format=j1" | jq '{
  temp_c: .current_condition[0].temp_C,
  desc:   .current_condition[0].weatherDesc[0].value
}'
```

Same for any language — the parser detects comment style from the shebang or file extension:

| Extension / Shebang | Comment prefix |
|---|---|
| `.sh`, `bash`, `sh`, `zsh` | `#` |
| `.py`, `python`, `python3` | `#` |
| `.js`, `node`, `deno` | `//` |
| `.rb`, `ruby` | `#` |
| `.lua` | `--` |
| `.ts`, `typescript` | `//` |
| `.r`, `Rscript` | `#` |
| `.pl`, `perl` | `#` |
| `.php` | `//` or `#` |
| Compiled binary (no extension) | none — requires `--agentos-manifest` flag |

### Annotation fields

| Annotation | Required | Default | Description |
|---|---|---|---|
| `@agentos tool:` | **yes** | — | Tool name (kebab-case). If absent, the file is ignored. |
| `@description:` | no | file name | Human + LLM readable description |
| `@permissions:` | no | `fs.data:r` | Comma-separated permission strings |
| `@risk:` | no | `readonly_scoped` | `readonly_scoped`, `readonly_external`, `write_scoped`, `exec_capable`, `control_plane` |
| `@timeout:` | no | `30` | Max execution seconds |
| `@version:` | no | `0.1.0` | Semver string |
| `@tags:` | no | `[]` | Comma-separated capability tags |
| `@network:` | no | `false` | `true` to allow outbound network in bwrap |
| `@input:` | no | — | Short description of expected input fields (free text, injected into tool description for LLM) |

### I/O contract

```
Input  → AGENTOS_INPUT environment variable (JSON string)
Output → stdout (JSON object)
Errors → non-zero exit code; stderr captured and returned as error reason
```

Scripts can also read `AGENTOS_TASK_ID`, `AGENTOS_AGENT_ID`, and `AGENTOS_DATA_DIR` from the environment.

---

## Architecture

```
$data_dir/scripts/        ← the "package manager"
  weather-lookup.sh       ← drop here = installed
  process-csv.py          ← edit here = hot-reloaded
  fetch-api.js            ← delete here = uninstalled

ScriptWatcher (notify)
  ↓  inotify Create/Modify/Delete events
ScriptParser
  ↓  ScriptAnnotations { name, description, permissions, ... }
ScriptTool (implements AgentTool)
  ↓  execute() → bwrap → AGENTOS_INPUT → stdout JSON
ToolRunner::register_dynamic()
  ↓  name_index updated
Agent system prompt
  ↓  tools_for_prompt() includes the script tool
Agent calls the tool
  ↓  same dispatch path as any built-in tool
```

---

## What AgentOS Calls This

From the outside: **Script Tools** — they appear in `agentos tool list` alongside built-in tools.  
The source module: `agentos-tools/src/script_tool.rs` + `agentos-kernel/src/script_watcher.rs`.  
The scripts directory: `$data_dir/scripts/` — configurable via `config.toml [tools] scripts_dir`.

---

## Phase Overview

| Phase | Name | Effort | Dependencies | Detail Doc | Status |
|---|---|---|---|---|---|
| 1 | ScriptParser + ScriptTool | 1d | None | [[01-script-parser-and-tool]] | complete |
| 2 | ScriptWatcher (inotify) | 1d | Phase 1 | [[02-script-watcher]] | complete |
| 3 | Kernel wiring + CLI | 1d | Phase 1, 2 | [[03-kernel-wiring-and-cli]] | complete |
| 4 | Agent manual integration | 0.5d | Phase 3 | [[04-agent-manual-integration]] | complete |

---

## Phase Dependency Graph

```mermaid
graph LR
    P1[Phase 1: ScriptParser + ScriptTool] --> P2[Phase 2: ScriptWatcher]
    P1 --> P3[Phase 3: Kernel wiring + CLI]
    P2 --> P3
    P3 --> P4[Phase 4: Agent manual]
```

---

## Key Design Decisions

1. **Env var for input, not stdin** — `AGENTOS_INPUT` is universally accessible in all languages without stream management. Stdin is left free for scripts that need interactive I/O internally.

2. **No separate TOML file** — The script IS the manifest. This eliminates the dual-file problem (manifest and script getting out of sync). If the annotation is missing, the file is silently skipped — no error.

3. **bwrap for isolation** — Same sandbox as `shell-exec`. Scripts get read-only `/usr`, `/bin`, `/lib`, tmpfs over `/root`, `/etc`, `/home`. The only writable path is `$data_dir`. Network off by default.

4. **`notify` not polling** — The watcher uses OS-native inotify (Linux), FSEvents (macOS), or ReadDirectoryChangesW (Windows). Sub-second latency, zero CPU when idle.

5. **Dynamic registration only** — Script tools live in `dynamic_tools: RwLock<HashMap>` in `ToolRunner`. They are NOT written to the `ToolRegistry` (manifest-based). This avoids trust tier verification for user-owned scripts (they already have file system access to write to `scripts/`).

6. **Scripts directory is user-owned** — Security model: if you can write to `$data_dir/scripts/`, you already have the equivalent capability. No signature required. This is the "local tool" trust model.

7. **Hot-reload on edit** — Modify/close events trigger re-parse and `register_dynamic` (overwrites existing). The running execution of the old version completes normally (the new `Arc<dyn AgentTool>` is only seen by future calls).

8. **Compiled binaries supported** — A binary with no extension can declare itself via a `--agentos-manifest` flag that prints JSON. `ScriptParser` detects this and calls the binary to extract annotations.

---

## Risks

| Risk | Mitigation |
|---|---|
| User script crashes kernel | Scripts run in subprocess via `bwrap`, never in-process |
| Script infinite loop | `@timeout:` annotation enforced via `tokio::time::timeout` |
| Path traversal in script name | `ScriptParser` validates name is valid kebab-case, no path separators |
| Script overwrites built-in tool name | `register_dynamic` checks static `tools` map first; conflict → warning, dynamic tool rejected |
| Sensitive env vars leaked to script | bwrap creates isolated namespaces; only `AGENTOS_INPUT`, `AGENTOS_TASK_ID`, `AGENTOS_AGENT_ID`, `AGENTOS_DATA_DIR` are explicitly passed |
