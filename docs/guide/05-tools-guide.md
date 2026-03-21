# Tools Guide

Agent Tools are the "programs" of AgentOS. Unlike traditional software designed for human interaction, Agent Tools are designed entirely for LLM consumption. They have no UI — they have a **machine-readable manifest** and a **typed interface**.

---

## How Tools Work

1. An LLM declares intent: _"I need to read file report.txt"_
2. The kernel parses the intent and finds the matching tool (`file-reader`)
3. The kernel checks the agent's capability token — does it have `fs.user_data:r`?
4. If authorized, the tool is executed (sandboxed when applicable)
5. The tool result is wrapped in delimiters and injected back into the LLM's context

```
LLM emits intent → Kernel receives IntentMessage
    → Capability check (permission required?)
    → Schema validation
    → Tool executed in sandbox
    → Result returned via Intent Channel
    → Output sanitized and injected into context
    → LLM receives: [TOOL_RESULT: file-reader] { content: "..." } [/TOOL_RESULT]
```

---

## Built-in Tools

AgentOS ships with 41 core tools (compiled into the kernel as native Rust). Use the `agent-manual` tool with `{"section": "tools"}` at runtime for the live list, or `{"section": "tool-detail", "name": "<tool>"}` for full schemas.

### File System

| Tool | Permission | Description |
|------|------------|-------------|
| `file-reader` | `fs.user_data:r` | Read files, list directories, with pagination (offset/limit) |
| `file-writer` | `fs.user_data:w` | Write files with create_only/overwrite modes and size guards |
| `file-editor` | `fs.user_data:w` | Apply line-range edits (insert, replace, delete) to existing files |
| `file-delete` | `fs.user_data:w` | Delete a file from the data directory |
| `file-move` | `fs.user_data:w` | Move or rename a file within the data directory |
| `file-diff` | `fs.user_data:r` | Compute unified diff between two files or between a file and a string |
| `file-glob` | `fs.user_data:r` | Find files matching a glob pattern |
| `file-grep` | `fs.user_data:r` | Search file contents by regex pattern |

### Memory

| Tool | Permission | Description |
|------|------------|-------------|
| `memory-search` | `memory.semantic:r` | Hybrid vector + FTS5 search across semantic or episodic memory |
| `memory-write` | `memory.semantic:w` | Write to semantic or episodic memory |
| `memory-read` | `memory.semantic:r` | Read a specific memory entry by key |
| `memory-delete` | `memory.semantic:w` | Delete a memory entry by key |
| `memory-stats` | `memory.semantic:r` | Memory usage statistics (counts, sizes per tier) |
| `memory-block-read` | `memory.blocks:r` | Read a named key-value memory block |
| `memory-block-write` | `memory.blocks:w` | Write or update a named memory block |
| `memory-block-list` | `memory.blocks:r` | List all named memory blocks |
| `memory-block-delete` | `memory.blocks:w` | Delete a named memory block |
| `archival-insert` | `memory.semantic:w` | Insert a large document into archival memory (chunked + indexed) |
| `archival-search` | `memory.semantic:r` | Search archival memory by query |
| `episodic-list` | `memory.episodic:r` | List episodic memory entries for a task |

### Procedural Memory

| Tool | Permission | Description |
|------|------------|-------------|
| `procedure-create` | `memory.procedural:w` | Record a reusable step-by-step procedure |
| `procedure-search` | `memory.procedural:r` | Search procedures by natural language query |
| `procedure-list` | `memory.procedural:r` | List all recorded procedures |
| `procedure-delete` | `memory.procedural:w` | Delete a procedure by ID |

### Network

| Tool | Permission | Description |
|------|------------|-------------|
| `http-client` | `network.outbound:x` | HTTP requests with secret injection, SSRF protection, and redirect control |
| `web-fetch` | `network.outbound:x` | Fetch a web page and extract text content (HTML stripped) |

### System & Process

| Tool | Permission | Description |
|------|------------|-------------|
| `shell-exec` | `process.exec:x` | Execute shell commands in bwrap sandbox with timeout and cancellation |
| `process-manager` | `process.list:x` | List and kill processes |
| `network-monitor` | `hal.devices:r` | Network interface stats |
| `hardware-info` | `hal.devices:r` | Hardware info: CPU, memory, disk, GPU |
| `log-reader` | `audit.read:r` | Read kernel and system log entries with filtering |

> **Warning:** `shell-exec` requires `bwrap` (bubblewrap) for sandbox isolation. It will refuse to run without it.

### Agent Coordination

| Tool | Permission | Description |
|------|------------|-------------|
| `agent-message` | `agent.message:x` | Send a direct message to another agent |
| `agent-list` | `agent.registry:r` | List registered agents and their status |
| `task-delegate` | `agent.message:x` | Delegate a sub-task to another agent (non-blocking) |
| `task-list` | `task.query:r` | List active and recent tasks |
| `task-status` | `task.query:r` | Inspect status of a specific task by ID |

### Data & Utilities

| Tool | Permission | Description |
|------|------------|-------------|
| `data-parser` | (none) | Parse JSON, CSV, TOML, YAML data |
| `think` | (none) | Private scratchpad for reasoning — output is NOT shown to the user |
| `datetime` | (none) | Get current date, time, timezone, and Unix timestamp |
| `agent-manual` | (none) | Query structured AgentOS documentation |
| `agent-self` | (none) | View own agent state: permissions, budget, tools, subscriptions |

---

## Tool Manifests

Every tool has a TOML manifest file describing its metadata, required permissions, sandbox constraints, and executor type. Built-in tool manifests live in `tools/core/`; user-installed manifests in `tools/user/`.

### Manifest Structure

```toml
[manifest]
name        = "file-reader"
version     = "1.0.0"
description = "Reads files from the data directory and returns their content as text"
author      = "agentos-core"

[capabilities_required]
permissions = ["fs.user_data:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "FileReadIntent"
output = "FileContent"

[sandbox]
network       = false      # Can this tool make network requests?
fs_write      = false      # Can this tool write to the filesystem?
gpu           = false      # Does this tool need GPU access?
max_memory_mb = 64         # Maximum memory allocation (MB)
max_cpu_ms    = 5000       # Maximum CPU time (ms)
syscalls      = []         # Allowed syscalls (empty = default restricted set)

# Optional — omit for built-in Rust tools
# [executor]
# type      = "inline"   # "inline" (default) or "wasm"
# wasm_path = ""         # path to .wasm file, relative to this manifest
```

---

## Installing Custom Tools

AgentOS supports two kinds of custom tools:

| Kind                       | When to use                                                |
| -------------------------- | ---------------------------------------------------------- |
| **Native Rust** (`inline`) | High-performance tools compiled into the kernel            |
| **WASM module** (`wasm`)   | Community tools written in any language, loaded at runtime |

### Installing a WASM Tool

```bash
agentctl tool install /path/to/my-tool.toml
```

The kernel will:

1. Validate the manifest format
2. Detect `executor.type = "wasm"` and load the `.wasm` file via the Wasmtime engine
3. Register the compiled WASM module in the tool runner — available immediately, no restart needed

### Listing Installed Tools

```bash
agentctl tool list
```

### Removing a Tool

```bash
agentctl tool remove my-tool
```

---

## Writing a WASM Tool

You can write a tool in **any language** that compiles to `wasm32-wasip1` — Rust, Python, Go, C, etc.

### The Protocol

The kernel communicates with your WASM tool using a simple 3-step protocol:

1. **Input**: Your tool reads its JSON payload from **stdin**
2. **Output**: Your tool writes its JSON result to the file path in the `AGENTOS_OUTPUT_FILE` environment variable
3. **Status**: Exit `0` on success, non-zero on failure. Debug logs go to **stderr** (captured in the audit log)

> **Why a file instead of stdout?**
> Two tools can execute in parallel. Each invocation gets a unique output file path (`{data_dir}/tool-out/{task_id}-{uuid}.json`), so parallel calls can never overwrite each other. The kernel reads the file after the module exits, then deletes it — cleanup is guaranteed even on failure.

### Example: Python Tool

```python
# weather_tool.py
import sys, json, os

# 1. Read the JSON payload the kernel sent
payload = json.loads(sys.stdin.read())
city = payload.get("city", "London")

# 2. Do your work
sys.stderr.write(f"Fetching weather for {city}\n")  # debug logs → audit log
result = {
    "city": city,
    "temperature_c": 22.0,
    "condition": "Sunny",
}

# 3. Write result to the unique output file the kernel injected
output_path = os.environ["AGENTOS_OUTPUT_FILE"]
with open(output_path, "w") as f:
    json.dump(result, f)

sys.exit(0)
```

Compile to WASM (requires [py2wasm](https://github.com/wasmerio/py2wasm) or MicroPython):

```bash
py2wasm weather_tool.py -o weather.wasm
```

### Example: Rust Tool

```rust
// src/main.rs
use std::{env, fs, io::Read};

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();

    let payload: serde_json::Value = serde_json::from_str(&input).unwrap();
    let city = payload["city"].as_str().unwrap_or("London");

    let result = serde_json::json!({
        "city": city,
        "temperature_c": 22.0,
        "condition": "Sunny"
    });

    let output_path = env::var("AGENTOS_OUTPUT_FILE").unwrap();
    fs::write(output_path, result.to_string()).unwrap();
}
```

Compile to WASM:

```bash
rustup target add wasm32-wasip1
cargo build --target wasm32-wasip1 --release
```

### Tool Manifest for a WASM Tool

```toml
[manifest]
name        = "weather-lookup"
version     = "1.0.0"
description = "Fetches current weather for a given city"
author      = "you"

[executor]
type      = "wasm"
wasm_path = "./weather.wasm"   # path relative to this manifest file

[capabilities_required]
permissions = ["network.outbound:x"]

[capabilities_provided]
outputs = ["weather.report"]

[intent_schema]
input  = "CityName"
output = "WeatherReport"

[sandbox]
network       = true    # needed for API calls
fs_write      = true    # needed to write AGENTOS_OUTPUT_FILE
max_memory_mb = 64
max_cpu_ms    = 5000
```

Then install it:

```bash
agentctl tool install /path/to/weather-lookup.toml
```

---

## The `AgentTool` Trait

All tools — whether native Rust or WASM-backed — implement the same `AgentTool` trait:

```rust
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// The tool's unique name (must match manifest name).
    fn name(&self) -> &str;

    /// List of permissions this tool requires.
    fn required_permissions(&self) -> Vec<(String, PermissionOp)>;

    /// Execute the tool with the given payload.
    /// The kernel has already validated permissions before calling this.
    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError>;
}
```

The `ToolExecutionContext` provides:

- `data_dir` — Sandboxed filesystem root for tool I/O
- `workspace_paths` — Additional directories the agent can access (configured by operator)
- `task_id` — ID of the task that triggered this tool call
- `trace_id` — Distributed trace ID for the audit log
- `cancellation_token` — Token that fires when the task is cancelled or timed out; long-running tools should check this via `tokio::select!`

---

## Tool Sandboxing

### Native Rust Tools

Safe built-in tools run in-process inside the kernel. Path traversal is prevented by validating all file paths at the Rust level before any I/O.

### WASM Tools

WASM tools run inside the **Wasmtime** runtime (from the Bytecode Alliance), which provides strong, capability-based isolation:

- The WASM module cannot access the host filesystem, network, or OS except through explicitly granted WASI capabilities
- CPU time is enforced via Wasmtime's **epoch interruption** — if `max_cpu_ms` is exceeded, the module is terminated
- Memory usage is limited to `max_memory_mb` via Wasmtime's linear memory limits
- The output file is cleaned up by a Rust RAII guard (`TempOutputFile`) even if the module crashes or times out

### Shell-Exec Tools

The `shell-exec` built-in runs in a `bwrap` (Bubblewrap) mount namespace — the shell process cannot see the host filesystem outside its allowed boundaries. seccomp-BPF syscall filtering is applied as an additional layer.
