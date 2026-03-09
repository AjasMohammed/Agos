---
title: Tool System
tags: [reference, tools]
---

# Tool System

Tools in AgentOS are the equivalent of programs in a traditional OS. They are designed for LLM consumption with machine-readable manifests and structured I/O.

## AgentTool Trait

Every tool implements this trait (`crates/agentos-tools/src/traits.rs`):

```rust
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError>;
    fn required_permissions(&self) -> Vec<(String, PermissionOp)>;
}
```

## Tool Execution Context

Passed to every tool invocation:

```rust
pub struct ToolExecutionContext {
    pub data_dir: PathBuf,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub trace_id: TraceID,
    pub permissions: PermissionSet,
    pub vault: Option<Arc<SecretsVault>>,
    pub hal: Option<Arc<HardwareAbstractionLayer>>,
}
```

## Built-in Tools

### File Operations
| Tool | Description | Permissions |
|---|---|---|
| `file-reader` | Read files from data directory (path traversal blocked) | `fs.user_data:r` |
| `file-writer` | Write files to data directory (creates subdirs) | `fs.user_data:w` |

### Memory
| Tool | Description | Permissions |
|---|---|---|
| `memory-search` | Vector + FTS5 hybrid search on semantic/episodic memory | `memory.semantic:r`, `memory.episodic:r` |
| `memory-write` | Store content with embeddings or episodic tags | `memory.semantic:w`, `memory.episodic:w` |

### Data Processing
| Tool | Description | Permissions |
|---|---|---|
| `data-parser` | Parse JSON, CSV, YAML, TOML, Markdown | None |

### Execution
| Tool | Description | Permissions |
|---|---|---|
| `shell-exec` | Execute shell commands in bwrap sandbox | `process.exec:x`, `fs.user_data:rw` |

### Communication
| Tool | Description | Permissions |
|---|---|---|
| `agent-message` | Send messages to other agents | `agent.message:w` |
| `task-delegate` | Delegate tasks to other agents | `agent.message:w` |

### Network
| Tool | Description | Permissions |
|---|---|---|
| `http-client` | Make HTTP requests (GET/POST/PUT/DELETE) | `network.outbound:x` |

### System
| Tool | Description | Permissions |
|---|---|---|
| `sys-monitor` | CPU, memory, load averages | `hardware.system:r` |
| `process-manager` | List/kill processes | `process.list:r`, `process.kill:x` |
| `log-reader` | Read system/application logs | `fs.app_logs:r` |
| `network-monitor` | Network interface stats | `network.logs:r` |
| `hardware-info` | Hardware details | `hardware.sensors:r` |

## Tool Manifests

Tools are defined by TOML manifest files in `tools/core/` (built-in) or `tools/user/` (custom):

```toml
[manifest]
name = "my-tool"
version = "1.0.0"
description = "What the tool does"
author = "author-name"

[capabilities_required]
permissions = ["fs.user_data:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input = "MyToolInput"
output = "MyToolOutput"

[sandbox]
network = false
fs_write = false
gpu = false
max_memory_mb = 128
max_cpu_ms = 10000
syscalls = []

[executor]
type = "inline"    # or "wasm"
wasm_path = "tool.wasm"  # for WASM tools
```

## WASM Tools

Custom tools can be compiled to WASM and executed via Wasmtime 38:

- Pre-compiled at kernel boot for fast startup
- Epoch-based CPU time limiting (no busy-polling)
- Sandboxed separately from native tools
- Protocol: JSON stdin → logic → write to `$AGENTOS_OUTPUT_FILE` → exit

See [[Security Model]] for sandbox details.

## Tool Loading Flow

1. Kernel boot scans `core_tools_dir` and `user_tools_dir`
2. Each `.toml` manifest is parsed into `ToolManifest`
3. Built-in tools are registered in `ToolRunner`
4. WASM tools are pre-compiled via `WasmToolExecutor`
5. All tools indexed by name in `ToolRegistry`

## Tool Execution Flow

```
LLM Response → Parse tool call → Validate capability token
    → Check permissions → Look up tool in registry
    → Execute tool (inline or WASM) → Return JSON result
    → Push result to context → Continue LLM loop
```

See [[Intent Processing Flow]] for full details.
