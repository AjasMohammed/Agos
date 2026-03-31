# MCP Client Hardening Design Spec

## Summary

Harden the AgentOS MCP client implementation from a thin prototype into a production-grade tool extensibility layer. This covers four concerns: transport abstraction (stdio + Streamable HTTP), supervised lifecycle management (health checks, backoff, hot-add/remove), security hardening (output sanitization, injection scanning, audit trail, per-server permissions), and adapter integration (config, CLI, kernel wiring).

**Scope:** Client mode only — AgentOS consuming tools from external MCP servers. Server mode (exposing AgentOS tools to external clients) is out of scope. MCP resources and prompts are deferred; only MCP tools are covered.

## Motivation

The current MCP implementation in `agentos-mcp` is a ~600 line prototype that works for happy-path demos but is not production-grade:

- **Single transport:** Stdio only. No way to connect to remote MCP servers over HTTP.
- **Fragile lifecycle:** Single-retry reconnect on crash. No health monitoring after boot. Crashed servers stay dead until kernel restart.
- **No security hardening:** MCP tool outputs enter the agent context window unsanitized. No audit trail. No rate limiting. No per-server permission scoping.
- **Sequential boot:** MCP servers spawn one at a time. A slow server blocks all subsequent servers.
- **Minimal config:** Only `name`, `command`, `args` per server. No timeout, env, permission, or transport options.

MCP is converging as the industry standard for tool interoperability (Claude Desktop, Cursor, VS Code, Windsurf all speak MCP). Making this the primary extensibility path — and making it robust — is more valuable than maintaining four half-built alternatives (WASM, native SDK, custom registry, MCP).

## Architecture

Four layers inside the `agentos-mcp` crate, each depending only on the one below:

```
+------------------------------------------+
|  Adapter Layer (McpToolAdapter)          |  AgentTool bridge
+------------------------------------------+
|  Security Layer (McpSecurityGate)        |  Output validation, audit, rate limit
+------------------------------------------+
|  Supervisor Layer (McpSupervisor)        |  Health, backoff, state machine
+------------------------------------------+
|  Transport Layer (McpTransport trait)    |  Stdio / Streamable HTTP
+------------------------------------------+
```

All layers remain modules within the existing `agentos-mcp` crate. No new crates are introduced.

---

## Layer 1: Transport

### Transport Trait

```rust
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and await the response.
    async fn send(&self, req: &JsonRpcRequest) -> Result<JsonRpcResponse, McpTransportError>;
    /// Gracefully close the connection.
    async fn close(&self) -> Result<(), McpTransportError>;
    /// Name of this transport for logging (e.g. "stdio:filesystem", "http:db-server").
    fn transport_name(&self) -> &str;
}
```

### Error Type

```rust
pub enum McpTransportError {
    /// Connection-level: broken pipe, process crash, HTTP connection refused.
    /// Supervisor should attempt reconnect.
    Connection(String),
    /// Protocol-level: JSON-RPC error from a live server.
    /// No reconnect needed.
    Protocol { code: i64, message: String },
    /// Timeout waiting for response.
    Timeout(Duration),
}
```

The error type split is the critical contract between transport and supervisor — `Connection` and `Timeout` trigger reconnect, `Protocol` does not.

### StdioTransport

Refactored from the current `McpClient`. Key changes:

- **No longer owns the initialize handshake.** That moves to the supervisor so it can be re-run on reconnect.
- **Per-server env vars and working directory** from config, merged with the existing safe env passthrough (`PATH`, `HOME`, `TMPDIR`).
- **Same bounded-read logic** (`read_line_limited`, `MAX_MCP_RESPONSE_BYTES = 10MB`).
- **Same connection mutex** pattern — write + read held together to prevent interleaving.
- **`close()`** sends SIGTERM to child process, waits 5s, then SIGKILL. Today `kill_on_drop` just kills immediately.

### StreamableHttpTransport

New implementation targeting the MCP `2025-03-26` spec revision.

- **Single endpoint:** `POST` to the configured URL with `Content-Type: application/json`.
- **Response modes:** Accepts either direct JSON response (`application/json`) or SSE upgrade (`text/event-stream`) for long-running calls. SSE stream is consumed until a `message` event with the JSON-RPC response arrives.
- **HTTP client:** `reqwest` with configurable timeout.
- **Authentication:** Optional `Authorization: Bearer <token>` header. Token is a vault secret reference resolved at connect time (e.g. `vault:mcp-db-token`).
- **`close()`** drops the HTTP client (no persistent connection to tear down, but cancels any in-flight requests).

### Transport Selection

Inferred from config: if `command` is set, use `StdioTransport`. If `url` is set, use `StreamableHttpTransport`. Setting both is a config validation error at boot.

---

## Layer 2: Supervisor

### State Machine

Each MCP server connection has one of four states:

```
     +----------+
     |Connecting+---------------+
     +----+-----+               |
          | success             | failure
          v                     v
     +----------+         +---------+
     |Connected |-------->|Backoff  |
     +----+-----+ error   +----+----+
          |                    | timer expires
          |                    v
          |               +----------+
          |               |Connecting| (retry)
          |               +----------+
          |
          | remove/shutdown
          v
     +----------+
     |Stopped   |
     +----------+
```

### McpSupervisor

One per kernel. Manages all MCP server connections:

```rust
pub struct McpSupervisor {
    /// All managed server connections, keyed by server name.
    servers: Arc<RwLock<HashMap<String, SupervisedServer>>>,
    /// Channel to send lifecycle events to the kernel (audit log, status).
    event_tx: mpsc::Sender<McpLifecycleEvent>,
    cancellation_token: CancellationToken,
}
```

### SupervisedServer

Per-server state:

```rust
pub struct SupervisedServer {
    config: McpServerResolvedConfig,
    transport: Arc<dyn McpTransport>,
    state: ServerState,          // Connecting | Connected | Backoff | Stopped
    tools: Vec<McpToolDef>,      // Cached from last successful list_tools
    stats: ServerStats,          // uptime, total_calls, failure_count, avg_latency_ms
    backoff: ExponentialBackoff, // 1s -> 2s -> 4s -> ... -> 5min cap
}
```

### Behaviors

- **Parallel boot:** `supervisor.start(configs)` spawns all configured servers concurrently via `join_all`. Failed servers enter `Backoff` state and will retry — they do not silently disappear.
- **Initialize handshake:** Owned by the supervisor. On connect/reconnect, sends `initialize` + `notifications/initialized`, then `tools/list` to refresh the tool cache.
- **Health loop:** Background tokio task on a configurable interval (default 30s). For each `Connected` server, sends `tools/list` as a lightweight health check. On `Connection` error, transitions to `Backoff` and emits a lifecycle event.
- **Exponential backoff:** Starts at 1s, doubles each attempt, caps at 5 minutes. Jitter added to prevent thundering herd. After 10 consecutive failures, stays in `Backoff` at the 5-minute cap — does not give up permanently.
- **Runtime hot-add:** `add_server(config)` creates transport, runs handshake, caches tools, and registers them with the `ToolRunner` via a provided callback. Callable from `KernelCommand::McpAdd`.
- **Runtime hot-remove:** `remove_server(name)` calls `transport.close()`, deregisters tools from `ToolRunner`, transitions to `Stopped`. Callable from `KernelCommand::McpRemove`.
- **Graceful shutdown:** On `CancellationToken` cancellation, iterates all servers, calls `close()` on each transport, emits `ServerStopped` events.
- **Tool refresh:** When a reconnect succeeds, `tools/list` is re-called. If the tool list changed (server was updated while disconnected), the supervisor deregisters old tools and registers new ones.

### Lifecycle Events

```rust
pub enum McpLifecycleEvent {
    ServerConnected { name: String, tool_count: usize },
    ServerDisconnected { name: String, error: String },
    ServerReconnecting { name: String, attempt: u32 },
    ServerStopped { name: String },
    ToolCallCompleted { server: String, tool: String, latency_ms: u64, success: bool },
}
```

Events are sent via an mpsc channel to the kernel, which writes them to the `AuditLog`.

### Replaces

`McpServerHandle` is entirely replaced by `SupervisedServer` + `McpSupervisor`. The handle was doing a subset of this (single-retry reconnect, connection state) without the state machine, backoff, health checks, or event emission.

---

## Layer 3: Security

### McpSecurityGate

Applied to every MCP tool call, between the supervisor and the adapter:

```rust
pub struct McpSecurityGate {
    audit_log: Arc<AuditLog>,
    injection_scanner: Arc<InjectionScanner>,
    /// Per-server rate limiters, keyed by server name.
    rate_limiters: RwLock<HashMap<String, SlidingWindowRateLimiter>>,
    /// Per-server config (max_response_bytes, allowed/denied tools, etc.).
    server_policies: RwLock<HashMap<String, McpServerPolicy>>,
}
```

Four concerns, executed in order on every tool call:

### 3a. Output Sanitization

- **Size limit:** Reject responses exceeding a configurable max (default 1MB, overridable per-server via `max_response_bytes`). The transport layer already caps at 10MB — this is the semantic limit for tool results consumed by the LLM.
- **Content type validation:** MCP tool results should be JSON or text. Reject binary blobs, base64 payloads exceeding 100KB, or deeply nested JSON (max depth 32).
- **Truncation with notice:** If a result exceeds the size limit but is valid text/JSON, truncate and append `[truncated: original size was X bytes]` rather than hard-failing.

### 3b. Injection Scanning

MCP tool outputs are untrusted external data entering the agent's reasoning context.

- Wrap MCP tool results in `<user_data>` tags that the system prompt instructs agents to treat as untrusted.
- Run the existing `InjectionScanner` (from `agentos-kernel/src/injection_scanner.rs`) on tool output text. If suspicious patterns are flagged, the result still passes through (blocking would be a DoS vector) but an `AuditEventType::InjectionAttempt` is logged with server name, tool name, and matched pattern.

### 3c. Audit Trail

Every MCP tool invocation gets an audit log entry:

```rust
AuditEntry {
    event_type: AuditEventType::McpToolCall,
    agent_id: Some(agent_id),
    task_id: Some(task_id),
    details: json!({
        "server": "filesystem",
        "tool": "read_file",
        "latency_ms": 42,
        "input_size_bytes": 128,
        "output_size_bytes": 4096,
        "success": true,
        "trust_tier": "community",
    }),
}
```

Failed calls are also logged with the error reason.

### 3d. Per-Server Permission Config

Config-driven security policy per MCP server:

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
trust_tier = "community"
max_response_bytes = 524288
rate_limit_rpm = 60
allowed_tools = ["read_file", "list_directory"]
denied_tools = ["write_file"]
```

- **`trust_tier`:** `"community"` (default) or `"verified"`. Controls sandbox policy enforcement.
- **`allowed_tools` / `denied_tools`:** Whitelist and blacklist. Deny takes precedence. Empty `allowed_tools` means allow all (except denied).
- **`rate_limit_rpm`:** Max calls per minute to this server. Enforced by the security gate before the call reaches the transport. Exceeded rate limit returns a structured error to the agent.
- **`max_response_bytes`:** Per-server override of the default 1MB output size limit.

---

## Layer 4: Adapter + Config + CLI + Kernel Wiring

### McpToolAdapter

Stays thin. Now delegates through security gate and supervisor:

```rust
pub struct McpToolAdapter {
    supervisor: Arc<McpSupervisor>,
    security_gate: Arc<McpSecurityGate>,
    server_name: String,
    tool_def: McpToolDef,
    permission: String,
}
```

Execution flow:

```
Agent calls tool
  -> ToolRunner validates capability token (unchanged)
  -> McpToolAdapter.execute()
    -> security_gate.check_rate_limit(server_name)
    -> supervisor.call_tool(server_name, tool_name, args)
      -> transport.send(request)
    -> security_gate.validate_output(response)
    -> security_gate.scan_injection(response)
    -> security_gate.audit_log(call_record)
    -> return sanitized result to agent
```

### Config

`McpServerConfig` expands:

```rust
pub struct McpServerConfig {
    // Identity
    pub name: String,

    // Stdio transport
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub working_dir: Option<PathBuf>,

    // HTTP transport
    pub url: Option<String>,
    pub auth_token: Option<String>,  // vault secret reference

    // Security
    pub trust_tier: Option<String>,
    pub max_response_bytes: Option<usize>,
    pub rate_limit_rpm: Option<u32>,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub timeout_secs: Option<u64>,

    // Lifecycle
    pub auto_reconnect: Option<bool>,
    pub health_check_interval_secs: Option<u64>,
}
```

Transport inferred: `command` set = stdio, `url` set = HTTP, both set = config error.

`auth_token` is a vault secret reference (e.g. `"vault:mcp-db-token"`), not plaintext.

### CLI

| Command | Description |
|---------|-------------|
| `agentctl mcp list` | Shows transport type (stdio/http), trust tier |
| `agentctl mcp status` | State (Connected/Backoff/Stopped), uptime, failure count, avg latency |
| `agentctl mcp add` | Hot-add a server at runtime with same config fields as TOML |
| `agentctl mcp remove` | Disconnect and deregister a server by name |
| `agentctl mcp test` | Dry-run: connect, initialize, list_tools, print results, disconnect |

### Kernel Wiring

- `McpSupervisor` replaces `mcp_handles: Arc<RwLock<Vec<Arc<McpServerHandle>>>>` on the `Kernel` struct.
- Boot sequence calls `supervisor.start(configs)` for parallel spawn.
- Supervisor health loop spawned as a background tokio task, cancelled via kernel `CancellationToken`.
- `McpLifecycleEvent`s forwarded to `AuditLog`.
- New `KernelCommand` variants: `McpAdd { config }`, `McpRemove { name }`, `McpStatus` (existing, adapted).

### New Dependencies

- `reqwest` — HTTP client for Streamable HTTP transport. Already used in the workspace by the `http-client` tool.
- No other new external dependencies.

---

## What This Replaces

| Current Code | Replaced By |
|-------------|-------------|
| `client.rs` (`McpClient`) | `transport/stdio.rs` (`StdioTransport`) |
| `handle.rs` (`McpServerHandle`) | `supervisor.rs` (`McpSupervisor` + `SupervisedServer`) |
| `adapter.rs` (`McpToolAdapter`) | `adapter.rs` (same name, new internals) |
| `types.rs` | `types.rs` (extended with new error types, lifecycle events) |
| `server.rs` (`McpServer`) | Unchanged (out of scope — server mode) |

## What This Does Not Change

- **MCP server mode** (`McpServer`, `agentctl mcp serve`) — untouched, out of scope.
- **AgentTool trait** — no changes to the tool interface.
- **ToolRunner** — no changes to how tools are dispatched; MCP tools remain `Box<dyn AgentTool>`.
- **Capability token system** — unchanged; MCP tools go through the same enforcement.
- **Trust tier definitions** — unchanged; MCP tools default to `Community`.

## Risks

| Risk | Mitigation |
|------|------------|
| `reqwest` adds compile-time weight | Already in workspace dep graph via http-client tool |
| Streamable HTTP spec is newer, less battle-tested | Stdio remains the default; HTTP is opt-in per server |
| Health check loop adds background load | Configurable interval, default 30s, uses lightweight `tools/list` |
| Hot-add/remove introduces concurrency complexity | `RwLock<HashMap>` for server map, state machine prevents invalid transitions |
| Per-server rate limiting needs clock precision | Use `tokio::time::Instant` sliding window, not wall clock |
