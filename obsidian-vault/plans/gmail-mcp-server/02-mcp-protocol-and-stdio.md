---
title: Phase 2 — MCP Protocol Core & Stdio Transport
tags:
  - mcp
  - protocol
  - phase-2
date: 2026-04-18
status: planned
effort: 1.5d
priority: high
---

# Phase 2 — MCP Protocol Core & Stdio Transport

> Implement MCP 2024-11-05 (or the spec version current at build time): handshake, tool registration, request/response loop, error mapping — all over stdio. Zero Gmail specifics.

---

## Why this phase

The server's contract with every MCP host is the protocol. Getting this right in isolation means every downstream phase just registers tools; nothing has to reach into transport concerns.

---

## Deliverables

- `gmail-mcp-core::mcp` module implementing JSON-RPC 2.0 server over stdio.
- `Tool` trait — authors define `name`, `description`, `input_schema`, `call(args) -> output`.
- `ToolRegistry` — holds `Arc<dyn Tool>` instances.
- `initialize` / `notifications/initialized` / `tools/list` / `tools/call` handlers.
- Clean shutdown on stdin EOF or SIGTERM.
- Error mapping: typed errors → JSON-RPC error codes.
- Protocol conformance test against the reference MCP test harness.

---

## Module layout

```
crates/gmail-mcp-core/src/
├── lib.rs
├── mcp/
│   ├── mod.rs
│   ├── protocol.rs       — JsonRpcRequest/Response/Error types
│   ├── transport.rs      — Transport trait + Stdio impl
│   ├── server.rs         — McpServer struct, dispatch loop
│   ├── tool.rs           — Tool trait + ToolRegistry
│   ├── schema.rs         — JSON Schema helpers
│   └── errors.rs         — McpError, mapping to JSON-RPC codes
└── ...
```

---

## Core types

```rust
// crates/gmail-mcp-core/src/mcp/protocol.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,              // always "2.0"
    #[serde(skip_serializing_if = "serde_json::Value::is_null", default)]
    pub id: serde_json::Value,        // number, string, or null (notifications)
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
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// MCP-specific
#[derive(Debug, Serialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Serialize, Default)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    // resources, prompts — not in v1
}

#[derive(Debug, Serialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}
```

---

## Tool trait

```rust
// crates/gmail-mcp-core/src/mcp/tool.rs
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

#[async_trait]
pub trait Tool: Send + Sync {
    /// Canonical tool name (lowercase, underscores, no spaces).
    fn name(&self) -> &str;

    /// Human-readable one-liner for model reasoning.
    fn description(&self) -> &str;

    /// JSON Schema describing the arguments object.
    fn input_schema(&self) -> Value;

    /// Execute the tool with validated arguments.
    async fn call(&self, args: Value) -> Result<Value, McpError>;
}

pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self { Self { tools: Vec::new() } }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        debug_assert!(
            !self.tools.iter().any(|t| t.name() == tool.name()),
            "duplicate tool name {}", tool.name()
        );
        self.tools.push(tool);
    }

    pub fn list(&self) -> Vec<ToolListItem> {
        self.tools.iter().map(|t| ToolListItem {
            name: t.name().to_string(),
            description: t.description().to_string(),
            input_schema: t.input_schema(),
        }).collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }
}
```

---

## Stdio transport

```rust
// crates/gmail-mcp-core/src/mcp/transport.rs
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct StdioTransport {
    stdin: BufReader<tokio::io::Stdin>,
    stdout: tokio::io::Stdout,
    line_buf: String,
}

impl StdioTransport {
    pub fn new() -> Self {
        Self {
            stdin: BufReader::new(tokio::io::stdin()),
            stdout: tokio::io::stdout(),
            line_buf: String::new(),
        }
    }

    /// Read one newline-delimited JSON-RPC message. `Ok(None)` on EOF.
    pub async fn recv(&mut self) -> std::io::Result<Option<JsonRpcRequest>> {
        self.line_buf.clear();
        let n = self.stdin.read_line(&mut self.line_buf).await?;
        if n == 0 {
            return Ok(None);
        }
        let req: JsonRpcRequest = serde_json::from_str(self.line_buf.trim())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(req))
    }

    pub async fn send(&mut self, resp: &JsonRpcResponse) -> std::io::Result<()> {
        let line = serde_json::to_string(resp)?;
        self.stdout.write_all(line.as_bytes()).await?;
        self.stdout.write_all(b"\n").await?;
        self.stdout.flush().await
    }
}
```

The transport is intentionally minimal. No buffering heuristics, no framing beyond newlines (per MCP spec).

---

## Server dispatch loop

```rust
// crates/gmail-mcp-core/src/mcp/server.rs
pub struct McpServer {
    registry: Arc<ToolRegistry>,
    server_info: ServerInfo,
    shutdown: tokio_util::sync::CancellationToken,
}

impl McpServer {
    pub fn new(registry: Arc<ToolRegistry>, server_info: ServerInfo) -> Self {
        Self {
            registry,
            server_info,
            shutdown: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub fn shutdown_token(&self) -> tokio_util::sync::CancellationToken {
        self.shutdown.clone()
    }

    pub async fn run(self, mut transport: StdioTransport) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                msg = transport.recv() => {
                    let req = match msg? {
                        Some(r) => r,
                        None => break, // EOF — host closed stdin
                    };
                    let resp = self.dispatch(req).await;
                    if let Some(r) = resp {
                        transport.send(&r).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn dispatch(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let is_notification = req.id.is_null();

        let result = match req.method.as_str() {
            "initialize" => self.on_initialize(req.params.clone()).await,
            "notifications/initialized" => { return None; }  // nothing to respond
            "tools/list" => self.on_list_tools().await,
            "tools/call" => self.on_call_tool(req.params.clone()).await,
            "ping" => Ok(serde_json::json!({})),
            m => Err(McpError::MethodNotFound(m.to_string())),
        };

        if is_notification {
            return None;
        }

        Some(match result {
            Ok(value) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: req.id,
                result: Some(value),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: req.id,
                result: None,
                error: Some(e.to_json_rpc_error()),
            },
        })
    }

    async fn on_initialize(&self, _params: Option<Value>) -> Result<Value, McpError> {
        let r = InitializeResult {
            protocol_version: "2024-11-05",
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: false }),
            },
            server_info: self.server_info.clone(),
        };
        Ok(serde_json::to_value(r)?)
    }

    async fn on_list_tools(&self) -> Result<Value, McpError> {
        Ok(serde_json::json!({ "tools": self.registry.list() }))
    }

    async fn on_call_tool(&self, params: Option<Value>) -> Result<Value, McpError> {
        #[derive(Deserialize)]
        struct ToolCallParams { name: String, arguments: Option<Value> }

        let p: ToolCallParams = serde_json::from_value(
            params.ok_or(McpError::InvalidParams("missing params".into()))?,
        ).map_err(|e| McpError::InvalidParams(e.to_string()))?;

        let tool = self.registry.get(&p.name)
            .ok_or_else(|| McpError::ToolNotFound(p.name.clone()))?;

        let args = p.arguments.unwrap_or(serde_json::json!({}));

        // Validate args against tool.input_schema() using `jsonschema` crate.
        validate_against_schema(&args, &tool.input_schema())?;

        let output = tool.call(args).await?;
        // MCP expects content array; we wrap a single JSON content block.
        Ok(serde_json::json!({
            "content": [{ "type": "json", "json": output }]
        }))
    }
}
```

---

## Error mapping

```rust
// crates/gmail-mcp-core/src/mcp/errors.rs
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("method not found: {0}")]
    MethodNotFound(String),
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("schema validation failed: {0}")]
    SchemaValidation(String),
    #[error("tool error: {0}")]
    ToolError(#[from] anyhow::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl McpError {
    pub fn to_json_rpc_error(&self) -> JsonRpcError {
        match self {
            Self::MethodNotFound(_)  => JsonRpcError { code: -32601, message: self.to_string(), data: None },
            Self::InvalidParams(_) |
            Self::SchemaValidation(_)=> JsonRpcError { code: -32602, message: self.to_string(), data: None },
            Self::ToolNotFound(_)    => JsonRpcError { code: -32602, message: self.to_string(), data: None },
            Self::ToolError(_)       => JsonRpcError { code: -32000, message: self.to_string(), data: None },
            Self::Serde(_)           => JsonRpcError { code: -32700, message: self.to_string(), data: None },
        }
    }
}
```

Tool-layer errors get their own mapping in Phase 7.

---

## Wire in main.rs

```rust
// crates/gmail-mcp/src/main.rs
use gmail_mcp_core::mcp::{McpServer, ServerInfo, StdioTransport, ToolRegistry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let registry = Arc::new(ToolRegistry::new());
    // Phases 4–7 register tools here.

    let server = McpServer::new(registry, ServerInfo {
        name: "gmail-mcp",
        version: env!("CARGO_PKG_VERSION"),
    });

    let shutdown = server.shutdown_token();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        shutdown.cancel();
    });

    server.run(StdioTransport::new()).await
}
```

---

## Dependencies added

```toml
# crates/gmail-mcp-core/Cargo.toml
[dependencies]
tokio.workspace = true
tokio-util = { version = "0.7", features = ["rt"] }
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
anyhow.workspace = true
async-trait = "0.1"
jsonschema = "0.17"     # args validation
tracing.workspace = true
```

---

## Test plan

1. Round-trip: `initialize` → response contains `protocolVersion = "2024-11-05"`, `serverInfo.name = "gmail-mcp"`.
2. `notifications/initialized` → no response emitted (notification).
3. `tools/list` with empty registry → `{ tools: [] }`.
4. `tools/list` with 2 registered tools → schema matches.
5. `tools/call` with unknown tool → error code `-32602`.
6. `tools/call` with args violating schema → error code `-32602`.
7. `tools/call` with tool returning `ToolError` → error code `-32000`.
8. Malformed JSON line → the process logs the error to stderr and continues reading (does not crash).
9. stdin EOF → loop exits cleanly, `run` returns `Ok`.
10. SIGINT → loop exits via shutdown token, returns `Ok`.

Fixture `Tool` implementations in `tests/common/mod.rs`:

```rust
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "return input" }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "msg": { "type": "string" } },
            "required": ["msg"]
        })
    }
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, McpError> {
        Ok(args)
    }
}
```

---

## Verification

```bash
cargo test -p gmail-mcp-core mcp::

# Conformance probe with a reference MCP client:
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}' \
  | ./target/debug/gmail-mcp serve

# Expected: JSON response on stdout with protocolVersion and serverInfo.
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/gmail-mcp-core/src/mcp/mod.rs` | New |
| `crates/gmail-mcp-core/src/mcp/protocol.rs` | New |
| `crates/gmail-mcp-core/src/mcp/transport.rs` | New |
| `crates/gmail-mcp-core/src/mcp/server.rs` | New |
| `crates/gmail-mcp-core/src/mcp/tool.rs` | New |
| `crates/gmail-mcp-core/src/mcp/schema.rs` | New |
| `crates/gmail-mcp-core/src/mcp/errors.rs` | New |
| `crates/gmail-mcp-core/src/lib.rs` | `pub mod mcp;` |
| `crates/gmail-mcp/src/main.rs` | Wire server |
| `crates/gmail-mcp-core/Cargo.toml` | Dependencies |

---

## Dependencies

- **Requires:** Phase 1.
- **Blocks:** Phase 4 (tools plug into the registry), Phase 7 (audit hooks into dispatch), Phase 8 (HTTP transport reuses dispatch).

---

## Related

- [[Gmail MCP Server Plan]]
- [[Gmail MCP Server Data Flow]]
- [[04-gmail-client-and-readonly-tools]]
