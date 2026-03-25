---
title: MCP Adapter Crate
tags:
  - mcp
  - tools
  - kernel
  - interoperability
  - plan
  - phase-2
date: 2026-03-24
status: planned
effort: 3d
priority: medium
---

# Phase 2 — MCP Adapter Crate

> Build `agentos-mcp`: a new crate that lets AgentOS import tools from any MCP-compliant server (client mode) and expose its own tools via the MCP protocol (server mode), solving vendor lock-in without paying an abstraction cost on the critical path.

---

## Why This Phase

The agent framework ecosystem is converging on the Model Context Protocol (MCP) — a JSON-RPC 2.0 standard for tool interoperability. AgentOS has a richer, more secure tool format (Ed25519 trust tiers, seccomp sandbox policies, intent schemas) but that richness creates a walled garden.

Without MCP:
- AgentOS cannot consume the growing ecosystem of MCP tool servers (filesystems, databases, APIs, browsers)
- Teams building tools must learn the AgentOS manifest format; no cross-framework tooling reuse
- Strategic risk: if MCP becomes the standard, AgentOS is isolated

The approach: **adapter, not replacement**. The `AgentTool` trait and trust-tier system remain canonical. MCP tools are wrapped as `AgentTool` impls at the boundary. AgentOS security (capability tokens, permission checks) still applies end-to-end.

---

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Tool format | Proprietary TOML + Ed25519 only | Also supports MCP-sourced tools via adapter |
| External tool servers | Not supported | MCP client: spawn + connect to MCP stdio/HTTP servers |
| Exporting tools | Not possible externally | MCP server: expose AgentOS ToolRunner via MCP protocol |
| Trust tier for MCP tools | N/A | `TrustTier::Community` (signed verification not possible for external tools) |
| Crate count | N crates | N + 1 (`agentos-mcp`) |

---

## Architecture

```
┌────────────────────────────────────────────────┐
│              agentos-mcp crate                 │
│                                                │
│  ┌──────────────┐     ┌───────────────────┐   │
│  │  McpClient   │     │    McpServer      │   │
│  │              │     │                   │   │
│  │ spawn_stdio()│     │ serve_stdio()     │   │
│  │ initialize() │     │ serve_http()      │   │
│  │ list_tools() │     │                   │   │
│  │ call_tool()  │     │ Accepts:          │   │
│  └──────┬───────┘     │  tools/list       │   │
│         │             │  tools/call       │   │
│  ┌──────▼───────┐     │  initialize       │   │
│  │McpToolAdapter│     └───────────────────┘   │
│  │              │                             │
│  │ impl AgentTool│                            │
│  │ TrustTier::   │                            │
│  │   Community   │                            │
│  └──────────────┘                             │
└────────────────────────────────────────────────┘
         │ registered in ToolRunner
         ▼
┌───────────────────────┐
│  agentos-kernel       │
│  ToolRunner           │
│  (existing)           │
└───────────────────────┘
```

---

## Crate Structure

```
crates/agentos-mcp/
├── Cargo.toml
├── src/
│   ├── lib.rs        — pub re-exports: McpClient, McpServer, McpToolAdapter
│   ├── types.rs      — JSON-RPC 2.0 + MCP message types
│   ├── client.rs     — McpClient: spawn MCP server process, send/receive RPC
│   ├── adapter.rs    — McpToolAdapter: wraps one MCP tool as AgentTool
│   └── server.rs     — McpServer: expose AgentOS ToolRunner via MCP protocol
└── tests/
    └── integration.rs — tests using a mock MCP server process
```

---

## Detailed Subtasks

### Subtask 2.1 — Create `crates/agentos-mcp/Cargo.toml`

```toml
[package]
name = "agentos-mcp"
version.workspace = true
edition.workspace = true

[dependencies]
agentos-types    = { path = "../agentos-types" }
agentos-tools    = { path = "../agentos-tools" }
serde            = { workspace = true, features = ["derive"] }
serde_json       = { workspace = true }
tokio            = { workspace = true, features = ["process", "io-util", "sync", "rt"] }
async-trait      = { workspace = true }
anyhow           = { workspace = true }
tracing          = { workspace = true }
tokio-util       = { version = "0.7", features = ["codec"] }
```

Add to workspace `Cargo.toml`:
```toml
[workspace]
members = [
    ...
    "crates/agentos-mcp",
]
```

---

### Subtask 2.2 — Define MCP types in `types.rs`

MCP uses a subset of JSON-RPC 2.0. Define only the types needed for `initialize`, `tools/list`, and `tools/call`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,           // always "2.0"
    pub id: serde_json::Value,     // integer or string
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// An MCP tool definition as returned by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,  // JSON Schema object
}

/// MCP initialize result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: serde_json::Value,
    #[serde(rename = "serverInfo")]
    pub server_info: McpServerInfo,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: &str, params: impl Serialize) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: serde_json::Value::Number(id.into()),
            method: method.to_string(),
            params: Some(serde_json::to_value(params).unwrap_or(serde_json::Value::Null)),
        }
    }
    pub fn new_no_params(id: u64, method: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: serde_json::Value::Number(id.into()),
            method: method.to_string(),
            params: None,
        }
    }
}
```

---

### Subtask 2.3 — Implement `McpClient` in `client.rs`

`McpClient` spawns an MCP server as a child process and communicates via newline-delimited JSON over stdio.

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use crate::types::{JsonRpcRequest, JsonRpcResponse, McpToolDef};

pub struct McpClient {
    stdin:    Mutex<ChildStdin>,
    stdout:   Mutex<BufReader<ChildStdout>>,
    _child:   Child,         // keep alive; kill_on_drop(true) on Child
    next_id:  AtomicU64,
}

impl McpClient {
    /// Spawn an MCP server process and perform the `initialize` handshake.
    pub async fn spawn_stdio(
        command: &str,
        args: &[&str],
    ) -> Result<Arc<Self>, anyhow::Error> {
        use tokio::process::Command;

        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;

        let stdin  = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());

        let client = Arc::new(Self {
            stdin:   Mutex::new(stdin),
            stdout:  Mutex::new(stdout),
            _child:  child,
            next_id: AtomicU64::new(1),
        });

        // Perform initialize handshake
        client.initialize().await?;
        Ok(client)
    }

    async fn send(&self, req: &JsonRpcRequest) -> Result<JsonRpcResponse, anyhow::Error> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');

        self.stdin.lock().await.write_all(line.as_bytes()).await?;

        let mut resp_line = String::new();
        self.stdout.lock().await.read_line(&mut resp_line).await?;

        let resp: JsonRpcResponse = serde_json::from_str(resp_line.trim())?;
        Ok(resp)
    }

    async fn initialize(&self) -> Result<(), anyhow::Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, "initialize", serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "agentos", "version": "1.0" }
        }));
        let resp = self.send(&req).await?;
        if resp.error.is_some() {
            anyhow::bail!("MCP initialize failed: {:?}", resp.error);
        }

        // Send initialized notification (no response expected)
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        });
        let mut line = serde_json::to_string(&notif)? + "\n";
        self.stdin.lock().await.write_all(line.as_bytes()).await?;
        Ok(())
    }

    /// List all tools advertised by this MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, anyhow::Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new_no_params(id, "tools/list");
        let resp = self.send(&req).await?;
        if let Some(err) = resp.error {
            anyhow::bail!("tools/list error: {} — {}", err.code, err.message);
        }
        let result = resp.result.unwrap_or(serde_json::Value::Null);
        let tools: Vec<McpToolDef> = serde_json::from_value(
            result.get("tools").cloned().unwrap_or(serde_json::Value::Array(vec![]))
        )?;
        Ok(tools)
    }

    /// Call a tool on the MCP server with the given arguments.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, anyhow::Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, "tools/call", serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        }));
        let resp = self.send(&req).await?;
        if let Some(err) = resp.error {
            anyhow::bail!("tools/call '{}' error: {} — {}", tool_name, err.code, err.message);
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}
```

---

### Subtask 2.4 — Implement `McpToolAdapter` in `adapter.rs`

Each MCP tool becomes an `AgentTool` with `TrustTier::Community`. Capability token validation happens in the AgentOS tool runner before `execute()` is called — MCP tools get the same permission enforcement as native tools.

```rust
use std::sync::Arc;
use async_trait::async_trait;
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use serde_json::Value;
use crate::client::McpClient;
use crate::types::McpToolDef;

pub struct McpToolAdapter {
    client:      Arc<McpClient>,
    tool_def:    McpToolDef,
    /// Permission resource required to use this tool (e.g. "mcp.filesystem").
    /// Operators can override per-tool by setting the permission in the agent's PermissionSet.
    permission:  String,
}

impl McpToolAdapter {
    pub fn new(
        client: Arc<McpClient>,
        tool_def: McpToolDef,
    ) -> Self {
        // Default permission: "mcp.<tool_name>" — operator grants this in agent perms.
        let permission = format!("mcp.{}", tool_def.name.replace('-', "_"));
        Self { client, tool_def, permission }
    }

    pub fn with_permission(mut self, permission: &str) -> Self {
        self.permission = permission.to_string();
        self
    }
}

#[async_trait]
impl AgentTool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.tool_def.name
    }

    fn description(&self) -> &str {
        &self.tool_def.description
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![(self.permission.clone(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: Value,
        _context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        // Permission was already validated by ToolRunner before this call.
        self.client
            .call_tool(&self.tool_def.name, payload)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: self.tool_def.name.clone(),
                reason: e.to_string(),
            })
    }
}
```

---

### Subtask 2.5 — Implement `McpServer` in `server.rs`

Expose all tools in a `ToolRunner` as an MCP server over stdio. This lets external MCP clients (e.g. Claude Desktop, Cursor) use AgentOS tools.

```rust
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use crate::types::{JsonRpcRequest, JsonRpcResponse, JsonRpcError, McpToolDef};

/// Serves AgentOS tools as an MCP server over stdin/stdout.
pub struct McpServer {
    tool_names: Vec<String>,
    tool_descriptions: Vec<String>,
    /// Callback to execute a tool by name with given arguments.
    /// In practice, this is a clone of `Arc<ToolRunner>` with a thin wrapper.
    executor: Arc<dyn McpToolExecutor>,
}

#[async_trait::async_trait]
pub trait McpToolExecutor: Send + Sync {
    async fn list_tools(&self) -> Vec<McpToolDef>;
    async fn call_tool(&self, name: &str, args: serde_json::Value)
        -> Result<serde_json::Value, String>;
}

impl McpServer {
    pub fn new(executor: Arc<dyn McpToolExecutor>) -> Self {
        Self { tool_names: vec![], tool_descriptions: vec![], executor }
    }

    /// Run the MCP server loop, reading from stdin and writing to stdout.
    /// Blocks until stdin is closed (caller process exits).
    pub async fn serve_stdio(&self) -> anyhow::Result<()> {
        let stdin  = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin).lines();
        let mut writer = tokio::io::BufWriter::new(stdout);

        while let Some(line) = reader.next_line().await? {
            let line = line.trim();
            if line.is_empty() { continue; }

            let req: JsonRpcRequest = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(e) => {
                    let err_resp = JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: serde_json::Value::Null,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32700,
                            message: format!("Parse error: {}", e),
                            data: None,
                        }),
                    };
                    let mut s = serde_json::to_string(&err_resp)?;
                    s.push('\n');
                    writer.write_all(s.as_bytes()).await?;
                    writer.flush().await?;
                    continue;
                }
            };

            let resp = self.handle_request(req).await;
            let mut s = serde_json::to_string(&resp)?;
            s.push('\n');
            writer.write_all(s.as_bytes()).await?;
            writer.flush().await?;
        }
        Ok(())
    }

    async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: Some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "agentos", "version": "1.0" }
                })),
                error: None,
            },
            "notifications/initialized" => {
                // notification — no response; skip
                // (caller should filter these out — but return a dummy to avoid hang)
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req.id,
                    result: Some(serde_json::Value::Null),
                    error: None,
                }
            }
            "tools/list" => {
                let tools = self.executor.list_tools().await;
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req.id,
                    result: Some(serde_json::json!({ "tools": tools })),
                    error: None,
                }
            }
            "tools/call" => {
                let (name, args) = match req.params.as_ref() {
                    Some(p) => (
                        p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        p.get("arguments").cloned().unwrap_or(serde_json::Value::Object(Default::default())),
                    ),
                    None => ("".to_string(), serde_json::Value::Object(Default::default())),
                };
                match self.executor.call_tool(&name, args).await {
                    Ok(result) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result: Some(serde_json::json!({
                            "content": [{ "type": "text", "text": result.to_string() }]
                        })),
                        error: None,
                    },
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32603,
                            message: e,
                            data: None,
                        }),
                    },
                }
            }
            _ => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", req.method),
                    data: None,
                }),
            },
        }
    }
}
```

---

### Subtask 2.6 — Kernel integration: load MCP servers from config

**File:** `config/default.toml` — add new section:

```toml
[mcp]
# List of MCP servers to connect at kernel boot.
# Each entry spawns a child process via stdio.
# servers = [
#   { name = "filesystem", command = "npx", args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"] },
# ]
servers = []
```

**File:** `crates/agentos-kernel/src/config.rs` — add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}
```

**File:** `crates/agentos-kernel/src/kernel.rs` — during boot, after tool registry is loaded:

```rust
// Connect to configured MCP servers and register their tools
for mcp_server_cfg in &config.mcp.servers {
    match agentos_mcp::McpClient::spawn_stdio(
        &mcp_server_cfg.command,
        &mcp_server_cfg.args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    ).await {
        Ok(client) => {
            match client.list_tools().await {
                Ok(tools) => {
                    for tool_def in tools {
                        let adapter = agentos_mcp::McpToolAdapter::new(
                            Arc::clone(&client),
                            tool_def,
                        );
                        tool_runner.register(Box::new(adapter));
                        tracing::info!(
                            mcp_server = %mcp_server_cfg.name,
                            "Registered MCP tool: {}", adapter.name()
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    mcp_server = %mcp_server_cfg.name,
                    error = %e,
                    "Failed to list MCP tools"
                ),
            }
        }
        Err(e) => tracing::warn!(
            mcp_server = %mcp_server_cfg.name,
            error = %e,
            "Failed to connect to MCP server"
        ),
    }
}
```

---

### Subtask 2.7 — CLI: expose MCP server mode

**File:** `crates/agentos-cli/src/commands/mcp.rs` (new file):

```rust
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum McpCommands {
    /// Start AgentOS as an MCP server (exposes all registered tools via MCP protocol on stdio).
    Serve,
    /// List configured MCP server connections.
    List,
}
```

This allows `agentctl mcp serve` to be used with Claude Desktop or other MCP clients.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-mcp/` | New crate — all files |
| `Cargo.toml` (workspace) | Add `crates/agentos-mcp` to `members` |
| `config/default.toml` | Add `[mcp]` section with empty `servers = []` |
| `crates/agentos-kernel/src/config.rs` | Add `McpConfig`, `McpServerConfig` |
| `crates/agentos-kernel/src/kernel.rs` | MCP server boot loop (connect + register tools) |
| `crates/agentos-kernel/Cargo.toml` | Add `agentos-mcp` dependency |
| `crates/agentos-cli/src/commands/mcp.rs` | New file — `McpCommands` |
| `crates/agentos-cli/src/main.rs` | Add `mcp` subcommand |

---

## Dependencies

None — independent from Phase 1.

---

## Test Plan

### Test 1 — `McpClient` can connect to a mock MCP server

```rust
#[tokio::test]
async fn test_mcp_client_initialize() {
    // Use a simple echo-style Python script as a mock MCP server
    let client = McpClient::spawn_stdio("python3", &["-c", MOCK_MCP_SERVER_SCRIPT]).await;
    assert!(client.is_ok(), "McpClient should initialize successfully");
}
```

Where `MOCK_MCP_SERVER_SCRIPT` is a minimal Python script that implements `initialize` + `tools/list` responses.

### Test 2 — `list_tools()` returns parsed `McpToolDef` list

```rust
#[tokio::test]
async fn test_mcp_list_tools() {
    let client = spawn_mock_client_with_tools(vec![
        McpToolDef {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        }
    ]).await;

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_file");
}
```

### Test 3 — `McpToolAdapter` implements `AgentTool` correctly

```rust
#[tokio::test]
async fn test_mcp_tool_adapter_name_and_permissions() {
    let adapter = McpToolAdapter::new(mock_client(), McpToolDef {
        name: "read_file".to_string(),
        description: "Read a file".to_string(),
        input_schema: json!({}),
    });

    assert_eq!(adapter.name(), "read_file");
    let perms = adapter.required_permissions();
    assert_eq!(perms.len(), 1);
    assert_eq!(perms[0].0, "mcp.read_file");
    assert_eq!(perms[0].1, PermissionOp::Execute);
}
```

### Test 4 — `McpServer` responds to `tools/list`

```rust
#[tokio::test]
async fn test_mcp_server_tools_list() {
    let executor = Arc::new(MockMcpExecutor::with_tools(vec!["ping"]));
    let server = McpServer::new(executor);

    let req = JsonRpcRequest::new_no_params(1, "tools/list");
    let resp = server.handle_request(req).await;

    assert!(resp.error.is_none());
    let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "ping");
}
```

### Test 5 — Unknown method returns JSON-RPC method-not-found error

```rust
#[tokio::test]
async fn test_mcp_server_unknown_method() {
    let server = McpServer::new(Arc::new(MockMcpExecutor::empty()));
    let req = JsonRpcRequest::new_no_params(99, "nonexistent/method");
    let resp = server.handle_request(req).await;
    assert!(resp.error.is_some());
    assert_eq!(resp.error.unwrap().code, -32601);
}
```

---

## Verification

```bash
# Build new crate and full workspace
cargo build -p agentos-mcp
cargo build --workspace

# Unit tests
cargo test -p agentos-mcp

# Full workspace tests
cargo test --workspace

# Clippy
cargo clippy --workspace -- -D warnings

# Format check
cargo fmt --all -- --check

# Integration: configure a real MCP server in config/default.toml and boot kernel
# Example: use the official MCP filesystem server
# In config/default.toml:
#   [[mcp.servers]]
#   name = "filesystem"
#   command = "npx"
#   args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
agentctl kernel start
agentctl tool list  # should show filesystem MCP tools registered
# Then have an agent use: read_file, write_file, list_directory

# Test server mode: pipe a tools/list request to agentctl mcp serve
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | agentctl mcp serve
# Should output a JSON-RPC response listing all registered AgentOS tools
```

---

## Related

- [[V3 Completion Plan]] — Master plan
- [[01-hal-registry-enforcement]] — Phase 1 (independent)
