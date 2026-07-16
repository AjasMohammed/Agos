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
| **Outbound** (kernel as server) | `agentos mcp serve` exposes all registered AgentOS tools as an MCP server over stdio — Claude Desktop, Cursor, and any MCP client can use AgentOS tools directly |

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
| `name` | `String` | yes | Human-readable label — used in logs and `agentos mcp status` |
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

This is transparent to the calling agent. Use `agentos mcp status` to see live connection state.

---

## Outbound: AgentOS as an MCP Server

### `agentos mcp serve`

Exposes all tools registered in the AgentOS tool registry as an MCP server over stdin/stdout. This is the bridge for Claude Desktop, Cursor, VS Code extensions, and any other MCP-compatible client to use AgentOS tools.

```bash
# Pipe stdin/stdout — used by MCP clients automatically
agentos mcp serve

# Test from the shell
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | agentos mcp serve
```

**Offline command** — does not require a running kernel. The tool registry is loaded fresh from tool manifest files.

The server grants a broad `operator_permissions()` permission set covering all 12 resource namespaces used by core tools:

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
      "command": "/path/to/agentos",
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
agentos mcp list
agentos --config /etc/agentos/prod.toml mcp list
```

Output format: one row per configured server showing `name`, `command`, and `args`.

### `mcp serve`

Start an MCP server on stdin/stdout exposing all AgentOS tools. **Offline.**

```bash
agentos mcp serve
```

### `mcp status`

Query the running kernel for live health of all configured MCP server connections. **Requires a running kernel.**

```bash
agentos mcp status
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

### `mcp attach` / `mcp detach`

Beyond `[[mcp.servers]]` config-driven boot, the kernel supports **runtime attach**: connect a new MCP server without restarting. Attachments are persisted to the kernel database (SQLite) and restored automatically on the next boot.

```bash
# Stdio transport — typical for npm/python MCP packages.
# The server command and its arguments go after `--`.
agentos mcp attach github -- npx -y @modelcontextprotocol/server-github

# HTTP transport with a static bearer token — for self-hosted MCP servers
agentos mcp attach corp-tools \
  --url https://mcp.internal.example.com \
  --token "$CORP_TOOLS_TOKEN"

# OAuth-protected server — token lifecycle managed by the kernel
agentos mcp attach jira \
  --url https://mcp.atlassian.example.com \
  --oauth-connector jira

# Remove a previously attached server
agentos mcp detach github
```

Pass environment variables to a stdio server with repeated `--env KEY=VALUE` flags (use `vault:SECRET_NAME` as the value to read a secret from the vault), e.g. `agentos mcp attach github --env GITHUB_TOKEN=vault:github_token -- npx -y @modelcontextprotocol/server-github`.

Both `attach` and `detach` are kernel-mediated: the change is applied to the running supervisor and persisted to SQLite. The kernel reports `McpStatus` for each running server (config-driven and runtime-attached) in a single list — confirm the result with `agentos mcp status`.

### `mcp tools` / `mcp call`

```bash
# List every tool in the local AgentOS tool registry (offline — loaded from
# tool manifest files, not the live kernel or attached servers)
agentos mcp tools

# Direct invocation of a single tool — bypasses the agent loop.
# Operates on the local AgentOS tool registry.
agentos mcp call --tool file-reader --input '{"path": "notes.txt"}'
```

`mcp tools` lists the local AgentOS tool registry offline; it does not query attached MCP servers. Both commands are useful for smoke-testing tool dispatch.

---

## OAuth Credentials

MCP servers behind an OAuth provider authenticate via the kernel's `OAuthTokenProvider`. The flow is:

1. `mcp attach --oauth-connector <name>` triggers the authorization-code flow if no valid token is in the vault. The kernel writes `OAuthFlowStarted` and prints a browser URL.
2. After the user consents, the kernel exchanges the code for tokens, encrypts them in the vault, and emits `OAuthCredentialStored` and `OAuthFlowCompleted`.
3. Each subsequent MCP call passes through `OAuthTokenProvider` which transparently refreshes the access token when expired and emits `OAuthTokenRefreshed`. Persistent failures emit `OAuthTokenExpired` and surface as `ToolExecutionFailed` to the agent.
4. `mcp detach` removes the credential and emits `OAuthCredentialDeleted`.

OAuth credentials live in the encrypted vault under the `mcp.oauth.<connector>` namespace and never appear in logs or audit details. Use `agentos secret list` to confirm storage; the value is opaque to the operator.

---

## MCP Security Gate

All output returned by external MCP tools passes through the `McpSecurityGate` before being injected into the agent context:

- **Injection scanning** — output is scanned for known prompt-injection patterns. Suspicious payloads emit a `McpInjectionDetected` audit event and are flagged in the result so the agent can treat the data as untrusted.
- **Per-server rate limiting** — a token bucket per server caps tool calls per minute to prevent runaway loops or quota exhaustion.
- **Risk class** — every dynamically registered MCP tool carries `risk_class = ReadonlyExternal`. The `ApprovalHook` may intercept the first call to require operator approval.

Treat MCP tool output the same way you treat any untrusted user data: use it as data, never as instructions to follow.

---

## A2A — Agent-to-Agent Protocol

Beyond classic MCP tool import, AgentOS speaks an **Agent-to-Agent** protocol so one AgentOS instance can discover and delegate tasks to agents on another instance.

```bash
# Show this agent's own A2A card (what external agents would see)
agentos a2a card

# Discover agents exposed by a remote AgentOS endpoint
agentos a2a discover https://other.example.com

# Delegate a capability invocation to a remote agent and wait for the result
agentos a2a delegate \
  --agent https://other.example.com \
  --capability researcher \
  --input '{"query":"Find the latest CVEs for openssl 3.x"}' \
  --wait

# List active A2A task delegations
agentos a2a tasks
```

The remote call is wrapped in a `task-delegate`-shaped intent and passes through the same `CapabilityToken` and `ApprovalHook` machinery as a local delegation. Failures (network, auth, capability) are returned as `ToolExecutionFailed` with the remote error embedded.

---

## Internals

### Key types

| Type | Crate | Purpose |
|------|-------|---------|
| `McpServerHandle` | `agentos-mcp` | Resilient connection wrapper with auto-reconnect and health state |
| `McpClient` | `agentos-mcp` | Raw stdio/HTTP connection holding a single `Mutex<McpConnection>` |
| `McpSupervisor` | `agentos-mcp` | Multi-server orchestrator that owns boot-time and runtime-attached servers in one place |
| `McpToolAdapter` | `agentos-mcp` | `AgentTool` implementation wrapping a single MCP tool via `McpServerHandle` |
| `McpSecurityGate` | `agentos-mcp` | Injection scanner and per-server rate limiter for MCP output |
| `OAuthTokenProvider` | `agentos-mcp` | OAuth credential lifecycle (auth-code flow, refresh, vault storage) |
| `McpServer` | `agentos-mcp` | Outbound server — serves `agentos mcp serve` |
| `A2AClient` / `A2ATaskExecutor` | `agentos-mcp` | Agent-to-Agent protocol client that wraps remote delegations as local tools |
| `McpServerConfig` | `agentos-kernel` | Config struct for `[[mcp.servers]]` entries |
| `KernelCommand::McpStatus` / `McpAttach` / `McpDetach` / `McpOAuthStore` | `agentos-bus` | Bus commands for the runtime MCP CLI |
| `McpServerStatus` | `agentos-bus` | Per-server health data (name, connected, tool_count, last_error) |

### McpServerHandle concurrency model

The handle uses two internal locks:
- `Mutex<Option<Arc<McpClient>>>` — held briefly to get/swap the live client reference; **never held during I/O**
- `Mutex<Option<String>>` — holds the last error string for `agentos mcp status`

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
