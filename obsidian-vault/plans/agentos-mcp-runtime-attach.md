---
title: MCP Runtime Attach/Detach
tags:
  - mcp
  - kernel
  - cli
  - phase-v3
  - plan
date: 2026-04-08
status: in-progress
effort: 2h
priority: medium
---

# MCP Runtime Attach/Detach

> Add `agentos mcp attach` and `agentos mcp detach` commands so operators can connect and disconnect MCP servers to a running kernel without restarting.

---

## Problem

MCP servers can only be configured at boot via `config/default.toml`. Connecting a new server requires editing the config file and restarting the kernel — impractical for interactive use.

## Decision

Extend the kernel with two new `KernelCommand` variants (`McpAttach`, `McpDetach`) and a `register_dynamic` path on `ToolRunner` that allows runtime tool registration without `&mut self`.

## Changes

| File | What changes |
|------|-------------|
| `crates/agentos-tools/src/runner.rs` | Add `dynamic_tools: RwLock<HashMap<String, Arc<dyn AgentTool>>>` + `register_dynamic` + update `execute`/`list_tools` |
| `crates/agentos-bus/src/message.rs` | Add `McpAttach`, `McpDetach` to `KernelCommand`; `McpAttached`, `McpDetached` to `KernelResponse` |
| `crates/agentos-kernel/src/commands/mcp.rs` | Add `cmd_mcp_attach` and `cmd_mcp_detach` |
| `crates/agentos-kernel/src/run_loop.rs` | Add dispatch arms |
| `crates/agentos-cli/src/commands/mcp.rs` | Add `Attach` and `Detach` subcommands + handlers |
| `crates/agentos-cli/src/main.rs` | Add match arms routing Attach/Detach through bus |

## CLI Usage

```bash
# stdio server
agentos mcp attach filesystem -- npx -y @modelcontextprotocol/server-filesystem /tmp

# HTTP server
agentos mcp attach remote --url http://localhost:8080/mcp --token mytoken

# detach
agentos mcp detach filesystem
```

## Consequences

- No restart required to add/remove MCP tools
- Dynamically registered tools are lost on kernel restart (not persisted to config)
- `ToolRunner` gains interior mutability via `RwLock` for dynamic tools only

## Related
[[MCP Integration]]
