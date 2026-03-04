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

AgentOS ships with the following core tools (compiled into the kernel as native Rust):

### `file-reader`

Read files from the data directory.

| Property            | Value                                  |
| ------------------- | -------------------------------------- |
| Permission required | `fs.user_data:r`                       |
| Sandbox             | No network, no filesystem write        |
| Input               | File path (relative to data directory) |
| Output              | File content as text                   |

### `file-writer`

Write files to the data directory. Creates subdirectories automatically.

| Property            | Value                 |
| ------------------- | --------------------- |
| Permission required | `fs.user_data:w`      |
| Sandbox             | No network            |
| Input               | File path + content   |
| Output              | Confirmation of write |

### `memory-search`

Search the semantic memory store by keyword.

| Property            | Value                     |
| ------------------- | ------------------------- |
| Permission required | `memory.semantic:r`       |
| Sandbox             | No network, no filesystem |
| Input               | Search query string       |
| Output              | Matching memory entries   |

### `memory-write`

Write an entry to the semantic memory store for long-term recall.

| Property            | Value                     |
| ------------------- | ------------------------- |
| Permission required | `memory.semantic:w`       |
| Sandbox             | No network                |
| Input               | Key + content to remember |
| Output              | Confirmation of write     |

### `data-parser`

Parse structured data formats.

| Property            | Value                             |
| ------------------- | --------------------------------- |
| Permission required | (none — read-only transformation) |
| Supported formats   | JSON, CSV                         |
| Input               | Raw data string + format hint     |
| Output              | Parsed structured data            |

### `shell-exec`

Execute shell commands in an isolated environment.

| Property            | Value                                             |
| ------------------- | ------------------------------------------------- |
| Permission required | `process.list:x` or `process.kill:x`              |
| Sandbox             | `bwrap` path-based isolation, restricted syscalls |
| Input               | Shell command string                              |
| Output              | Command stdout/stderr and exit code               |

> **Warning:** This tool is highly restricted. It uses `bwrap` (bubblewrap) for path-based isolation to prevent agents from accessing the host filesystem beyond designated boundaries.

### `agent-message`

Send a message to another agent via the Agent Message Bus.

| Property            | Value                               |
| ------------------- | ----------------------------------- |
| Permission required | `agent.message:x`                   |
| Input               | Target agent name + message content |
| Output              | Delivery confirmation               |

### `task-delegate`

Delegate a subtask to another agent and wait for the result.

| Property            | Value                                |
| ------------------- | ------------------------------------ |
| Permission required | `agent.message:x`                    |
| Input               | Target agent name + task description |
| Output              | Delegated task result                |

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
- `task_id` — ID of the task that triggered this tool call
- `trace_id` — Distributed trace ID for the audit log

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
