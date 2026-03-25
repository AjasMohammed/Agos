---
title: MCP Integration
tags:
  - mcp
  - tools
  - integration
  - handbook
date: 2026-03-25
status: complete
effort: reference
priority: medium
---

# MCP Integration

> Connect any MCP-compatible tool server to AgentOS, and expose AgentOS tools to any MCP client — bridging the LLM tool ecosystem with AgentOS's security model.

---

## Overview

AgentOS has bidirectional MCP (Model Context Protocol) support:

| Direction | What it does |
|-----------|--------------|
| **Inbound** (kernel consumer) | Kernel spawns external MCP servers at boot, discovers their tools, and makes them available to agents with full capability-token enforcement |
| **Outbound** (kernel as server) | `agentctl mcp serve` exposes all registered AgentOS tools as an MCP server over stdio — Claude Desktop, Cursor, and any MCP client can use AgentOS tools directly |

MCP uses JSON-RPC 2.0 over stdio. Each server is a child process; communication is line-delimited JSON.

---

## Inbound: Connecting External MCP Servers

### Configuration

Add servers to `config/default.toml`:

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[[mcp.servers]]
name = "web-search"
command = "python3"
args = ["-m", "mcp_server_websearch"]
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | `String` | yes | Human-readable label — used in logs and `agentctl mcp status` |
| `command` | `String` | yes | Executable to spawn (`npx`, `python3`, absolute path, etc.) |
| `args` | `[String]` | no | Arguments passed to `command` |

Both `name` and `command` must be non-empty — the kernel validates this at boot and will refuse to start if either is blank.

### Boot-time behavior

At kernel startup, for each configured server:

1. The server process is spawned with `kill_on_drop(true)` — the process is automatically killed if the kernel exits.
2. The MCP initialize handshake runs over stdio.
3. `tools/list` is called to discover available tools.
4. Each tool is registered with `ToolRunner` under its original name.
5. **Name collision protection**: any MCP tool whose name matches an existing AgentOS tool (or another tool from the same server) is skipped with a warning.

Failures at any step are logged as warnings and **do not abort boot** — a missing MCP server doesn't take down the kernel.

### Security model

MCP tools go through the same security pipeline as native tools:

- Every call goes through `ToolRunner`, which validates the agent's `CapabilityToken` and `PermissionSet` before calling the adapter.
- Each MCP tool requires the permission `mcp.<sanitized_name>:x` by default, where the tool name has non-alphanumeric characters replaced with `_` (e.g. `fs:read` → `mcp.fs_read`).
- Environment isolation: the server process inherits only `PATH`, `HOME`, `TMPDIR`, `TEMP`, `TMP` — other environment variables (API keys, etc.) are NOT passed through.

### Auto-reconnect

If an MCP server process crashes or its stdio connection breaks, the kernel detects the failure on the next tool call and automatically:

1. Re-spawns the server process.
2. Retries the tool call once against the fresh process.
3. If reconnect fails, returns a `ToolExecutionFailed` error to the agent.

This is transparent to the calling agent. Use `agentctl mcp status` to see live connection state.

---

## Outbound: AgentOS as an MCP Server

### `agentctl mcp serve`

Exposes all tools registered in the AgentOS tool registry as an MCP server over stdin/stdout. This is the bridge for Claude Desktop, Cursor, VS Code extensions, and any other MCP-compatible client to use AgentOS tools.

```bash
# Pipe stdin/stdout — used by MCP clients automatically
agentctl mcp serve

# Test from the shell
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | agentctl mcp serve
```

**Offline command** — does not require a running kernel. The tool registry is loaded fresh from tool manifest files.

The server grants a broad `operator_permissions()` permission set covering all 11 resource namespaces used by core tools:

| Namespace | Access |
|-----------|--------|
| `fs:` | read, write, execute |
| `fs.user_data` | read, write |
| `memory.` | read, write |
| `net:` | read, write, execute |
| `network.` | read, execute |
| `hardware.` | read |
| `process.` | read, execute |
| `task.` | read |
| `escalation.` | read, query |
| `user.` | read, write, execute |
| `agent.` | read, execute |
| `data.` | read, write |

### Claude Desktop integration

Add to Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "agentos": {
      "command": "/path/to/agentctl",
      "args": ["--config", "/path/to/config/default.toml", "mcp", "serve"]
    }
  }
}
```

---

## CLI Reference

### `mcp list`

List all MCP servers configured in the current config file. **Offline** — shows config only, not live state.

```bash
agentctl mcp list
agentctl --config /etc/agentos/prod.toml mcp list
```

Output format: one row per configured server showing `name`, `command`, and `args`.

### `mcp serve`

Start an MCP server on stdin/stdout exposing all AgentOS tools. **Offline.**

```bash
agentctl mcp serve
```

### `mcp status`

Query the running kernel for live health of all configured MCP server connections. **Requires a running kernel.**

```bash
agentctl mcp status
```

Sample output:

```
NAME                 STATUS       TOOLS    LAST ERROR
----------------------------------------------------------------------
filesystem           connected    8        -
web-search           disconnected 0        MCP server 'web-search' reconnect failed: ...
```

| Column | Description |
|--------|-------------|
| `NAME` | Server name from config |
| `STATUS` | `connected` if the process is alive; `disconnected` if the last connection attempt failed |
| `TOOLS` | Number of tools registered from this server at boot |
| `LAST ERROR` | Last connection-level error message, or `-` if none |

---

## Internals

### Key types

| Type | Crate | Purpose |
|------|-------|---------|
| `McpServerHandle` | `agentos-mcp` | Resilient connection wrapper with auto-reconnect and health state |
| `McpClient` | `agentos-mcp` | Raw stdio connection holding a single `Mutex<McpConnection>` |
| `McpToolAdapter` | `agentos-mcp` | `AgentTool` implementation wrapping a single MCP tool via `McpServerHandle` |
| `McpServer` | `agentos-mcp` | Outbound server — serves `agentctl mcp serve` |
| `McpServerConfig` | `agentos-kernel` | Config struct for `[[mcp.servers]]` entries |
| `KernelCommand::McpStatus` | `agentos-bus` | Bus command for `agentctl mcp status` |
| `McpServerStatus` | `agentos-bus` | Per-server health data (name, connected, tool_count, last_error) |

### McpServerHandle concurrency model

The handle uses two internal locks:
- `Mutex<Option<Arc<McpClient>>>` — held briefly to get/swap the live client reference; **never held during I/O**
- `Mutex<Option<String>>` — holds the last error string for `agentctl mcp status`

The `McpClient::conn` mutex serializes actual reads and writes, ensuring request/response pairs never interleave across concurrent calls.

### Connection error detection

The following error message substrings trigger a reconnect:
- `"closed connection"`, `"did not respond"`, `"Failed to spawn"`, `"broken pipe"`, `"BrokenPipe"`

Protocol-level errors (JSON-RPC error responses from a live server) pass through without triggering reconnect.

---

## Related

- [[04-CLI Reference Complete]] — full CLI reference including `mcp` commands
- [[07-Tool System]] — how tools are registered, trusted, and executed
- [[08-Security Model]] — capability tokens and permission enforcement
- [[16-Configuration Reference]] — `[mcp]` config section
