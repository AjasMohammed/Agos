---
title: "Phase 1: MCP Transport Layer"
tags:
  - mcp
  - v3
  - plan
  - phase-1
date: 2026-03-30
status: planned
effort: 1.5d
priority: high
---

# Phase 1: MCP Transport Layer

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the stdio transport from `McpClient`, define the `McpTransport` trait, and add `StreamableHttpTransport` for remote MCP servers.

**Architecture:** A trait `McpTransport` with two implementations (`StdioTransport`, `StreamableHttpTransport`). The trait is the contract between the transport layer and the supervisor layer above it. Error types carry reconnect semantics.

**Tech Stack:** Rust, tokio, async-trait, reqwest (workspace dep), serde_json

---

## Why This Phase

The current `McpClient` hardcodes stdio as the only transport and owns the initialize handshake. This phase separates transport mechanics from connection lifecycle, enabling the supervisor (Phase 2) to manage connections transport-agnostically. Adding HTTP transport unlocks remote MCP servers.

## Current State

- `crates/agentos-mcp/src/client.rs` — `McpClient` struct (line 83) with `spawn_stdio()`, `send()`, `initialize()`, `list_tools()`, `call_tool()`
- `read_line_limited()` helper (line 29) and `MAX_MCP_RESPONSE_BYTES` constant (line 22)
- No transport abstraction, no HTTP support
- Initialize handshake is inside `McpClient::spawn_stdio()` (line 143)

## Target State

- New module `crates/agentos-mcp/src/transport.rs` with `McpTransport` trait and `McpTransportError` enum
- New module `crates/agentos-mcp/src/transport/stdio.rs` with `StdioTransport` (refactored from `McpClient`)
- New module `crates/agentos-mcp/src/transport/http.rs` with `StreamableHttpTransport`
- `client.rs` kept temporarily for `McpServer` (server mode) which still uses it — removed in a later cleanup
- `read_line_limited()` and `MAX_MCP_RESPONSE_BYTES` moved to a shared `transport/util.rs`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-mcp/Cargo.toml` | Add `reqwest` workspace dep |
| `crates/agentos-mcp/src/transport.rs` | Create — trait + error type + re-exports |
| `crates/agentos-mcp/src/transport/mod.rs` | Create — module declarations |
| `crates/agentos-mcp/src/transport/stdio.rs` | Create — `StdioTransport` |
| `crates/agentos-mcp/src/transport/http.rs` | Create — `StreamableHttpTransport` |
| `crates/agentos-mcp/src/transport/util.rs` | Create — `read_line_limited()`, constants |
| `crates/agentos-mcp/src/lib.rs` | Add `pub mod transport`, keep existing modules |

## Dependencies

- **Requires:** Nothing (first phase)
- **Blocks:** Phase 2 (Supervisor), Phase 3 (Security)

---

### Task 1: Transport Error Type and Trait

**Files:**
- Create: `crates/agentos-mcp/src/transport/mod.rs`

- [ ] **Step 1: Write the failing test**

Create the transport module with the trait, error enum, and a basic test:

```rust
// crates/agentos-mcp/src/transport/mod.rs

pub mod http;
pub mod stdio;
mod util;

pub use util::{read_line_limited, MAX_MCP_RESPONSE_BYTES};

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;

use crate::types::{JsonRpcRequest, JsonRpcResponse};

/// Error type for MCP transport operations.
///
/// The variant determines reconnect behavior in the supervisor:
/// - `Connection` and `Timeout` → supervisor should attempt reconnect
/// - `Protocol` → server is alive, no reconnect needed
#[derive(Debug)]
pub enum McpTransportError {
    /// Connection-level failure: broken pipe, process crash, HTTP connection refused.
    Connection(String),
    /// Protocol-level error: JSON-RPC error from a live server.
    Protocol { code: i64, message: String },
    /// Timeout waiting for response.
    Timeout(Duration),
}

impl fmt::Display for McpTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(msg) => write!(f, "MCP connection error: {}", msg),
            Self::Protocol { code, message } => {
                write!(f, "MCP protocol error (code {}): {}", code, message)
            }
            Self::Timeout(d) => write!(f, "MCP request timed out after {:?}", d),
        }
    }
}

impl std::error::Error for McpTransportError {}

impl McpTransportError {
    /// Returns `true` if this error indicates reconnection should be attempted.
    pub fn should_reconnect(&self) -> bool {
        matches!(self, Self::Connection(_) | Self::Timeout(_))
    }
}

/// Abstraction over how bytes move between AgentOS and an MCP server.
///
/// Implementations handle the wire protocol (stdio pipes, HTTP POST, etc.)
/// but do NOT own the MCP initialize handshake — that is the supervisor's job.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and await the response.
    async fn send(&self, req: &JsonRpcRequest) -> Result<JsonRpcResponse, McpTransportError>;

    /// Gracefully close the connection.
    async fn close(&self) -> Result<(), McpTransportError>;

    /// Human-readable transport name for logging (e.g. "stdio:filesystem").
    fn transport_name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_should_reconnect() {
        assert!(McpTransportError::Connection("broken pipe".into()).should_reconnect());
        assert!(McpTransportError::Timeout(Duration::from_secs(30)).should_reconnect());
        assert!(!McpTransportError::Protocol {
            code: -32603,
            message: "internal error".into()
        }
        .should_reconnect());
    }

    #[test]
    fn transport_error_display() {
        let conn = McpTransportError::Connection("broken pipe".into());
        assert!(conn.to_string().contains("broken pipe"));

        let proto = McpTransportError::Protocol {
            code: -32601,
            message: "method not found".into(),
        };
        assert!(proto.to_string().contains("-32601"));

        let timeout = McpTransportError::Timeout(Duration::from_secs(30));
        assert!(timeout.to_string().contains("30"));
    }
}
```

- [ ] **Step 2: Create the util module with shared helpers**

Extract `read_line_limited` and `MAX_MCP_RESPONSE_BYTES` from `client.rs` into a shared util:

```rust
// crates/agentos-mcp/src/transport/util.rs

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Maximum number of bytes accepted from a single MCP server response line.
/// Prevents memory exhaustion from a malicious or malfunctioning server.
pub const MAX_MCP_RESPONSE_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// Read a single newline-terminated line from `reader`, enforcing a byte limit
/// *during* the read rather than after. This prevents a malicious server from
/// exhausting memory by sending a very large payload without a newline.
///
/// Returns the number of bytes read (0 means EOF).
pub async fn read_line_limited(
    reader: &mut (impl AsyncBufRead + Unpin),
    buf: &mut String,
    max_bytes: usize,
) -> Result<usize, anyhow::Error> {
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            break; // EOF
        }
        let newline_pos = available.iter().position(|&b| b == b'\n');
        let chunk_end = newline_pos.map_or(available.len(), |p| p + 1);
        total += chunk_end;
        if total > max_bytes {
            anyhow::bail!("MCP server response exceeds {} byte limit", max_bytes);
        }
        let chunk = &available[..chunk_end];
        buf.push_str(
            std::str::from_utf8(chunk)
                .map_err(|e| anyhow::anyhow!("Invalid UTF-8 from MCP server: {e}"))?,
        );
        reader.consume(chunk_end);
        if newline_pos.is_some() {
            break; // found the line terminator
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn read_line_limited_normal() {
        let data = b"hello world\n";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf, 1024).await.unwrap();
        assert_eq!(n, 12);
        assert_eq!(buf, "hello world\n");
    }

    #[tokio::test]
    async fn read_line_limited_exceeds_limit() {
        let data = b"this is too long\n";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();
        let result = read_line_limited(&mut reader, &mut buf, 5).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn read_line_limited_eof() {
        let data = b"";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf, 1024).await.unwrap();
        assert_eq!(n, 0);
    }
}
```

- [ ] **Step 3: Wire transport module into lib.rs**

Add `pub mod transport;` to `crates/agentos-mcp/src/lib.rs` and re-export key types:

```rust
// Add after existing pub mod lines:
pub mod transport;

// Add to re-exports:
pub use transport::{McpTransport, McpTransportError};
```

- [ ] **Step 4: Run tests to verify**

Run: `cargo test -p agentos-mcp`
Expected: All new tests pass alongside existing tests.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-mcp/src/transport/
git add crates/agentos-mcp/src/lib.rs
git commit -m "feat(mcp): add McpTransport trait, McpTransportError, and util module"
```

---

### Task 2: StdioTransport

**Files:**
- Create: `crates/agentos-mcp/src/transport/stdio.rs`

- [ ] **Step 1: Write the StdioTransport implementation**

Refactored from `McpClient` in `client.rs`. Key differences:
- No `initialize()` — that moves to the supervisor
- `close()` sends SIGTERM, waits 5s, then SIGKILL
- Configurable env vars and working directory
- `next_id` counter for JSON-RPC IDs

```rust
// crates/agentos-mcp/src/transport/stdio.rs

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use super::util::{read_line_limited, MAX_MCP_RESPONSE_BYTES};
use super::McpTransportError;
use crate::types::{JsonRpcRequest, JsonRpcResponse};

/// Default timeout for a single MCP request/response round-trip.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Holds the stdin/stdout pair for an MCP subprocess connection.
struct StdioConnection {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// Stdio-based MCP transport — spawns a child process and communicates via
/// newline-delimited JSON over stdin/stdout.
///
/// Does NOT perform the MCP initialize handshake. That is the supervisor's
/// responsibility, so it can be re-run on reconnect.
pub struct StdioTransport {
    /// Display name for logging.
    name: String,
    /// Executable to spawn.
    command: String,
    /// Arguments passed to the command.
    args: Vec<String>,
    /// Extra environment variables to pass to the subprocess.
    extra_env: HashMap<String, String>,
    /// Working directory for the subprocess.
    working_dir: Option<PathBuf>,
    /// Per-request timeout.
    timeout: Duration,
    /// Stdin/stdout pair, protected by mutex to prevent interleaving.
    conn: Mutex<StdioConnection>,
    /// Child process handle — kept alive for the transport's lifetime.
    child: Mutex<Child>,
    /// Monotonically increasing JSON-RPC ID counter.
    next_id: AtomicU64,
}

impl StdioTransport {
    /// Spawn the MCP server subprocess and return a ready transport.
    ///
    /// Only a minimal set of environment variables is inherited (`PATH`, `HOME`,
    /// `TMPDIR`). Additional vars can be passed via `extra_env`.
    pub async fn spawn(
        name: String,
        command: String,
        args: Vec<String>,
        extra_env: HashMap<String, String>,
        working_dir: Option<PathBuf>,
        timeout_secs: Option<u64>,
    ) -> Result<Self, McpTransportError> {
        let safe_env: Vec<(String, String)> = ["PATH", "HOME", "TMPDIR", "TEMP", "TMP"]
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();

        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env_clear()
            .envs(safe_env)
            .envs(extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .kill_on_drop(true);

        if let Some(ref dir) = working_dir {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|e| {
            McpTransportError::Connection(format!(
                "Failed to spawn MCP server '{}': {}",
                command, e
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            McpTransportError::Connection(format!(
                "Failed to acquire stdin pipe for '{}'",
                command
            ))
        })?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| {
            McpTransportError::Connection(format!(
                "Failed to acquire stdout pipe for '{}'",
                command
            ))
        })?);

        Ok(Self {
            name,
            command,
            args,
            extra_env,
            working_dir,
            timeout: Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)),
            conn: Mutex::new(StdioConnection { stdin, stdout }),
            child: Mutex::new(child),
            next_id: AtomicU64::new(1),
        })
    }

    /// The command used to spawn this transport (for reconnect logging).
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The arguments used to spawn this transport.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The extra env vars configured for this transport.
    pub fn extra_env(&self) -> &HashMap<String, String> {
        &self.extra_env
    }

    /// The working directory configured for this transport.
    pub fn working_dir(&self) -> Option<&PathBuf> {
        self.working_dir.as_ref()
    }

    /// The timeout configured for this transport.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[async_trait]
impl super::McpTransport for StdioTransport {
    async fn send(&self, req: &JsonRpcRequest) -> Result<JsonRpcResponse, McpTransportError> {
        let mut conn = self.conn.lock().await;

        let mut line = serde_json::to_string(req).map_err(|e| {
            McpTransportError::Connection(format!("Failed to serialize request: {}", e))
        })?;
        line.push('\n');

        conn.stdin.write_all(line.as_bytes()).await.map_err(|e| {
            McpTransportError::Connection(format!("Failed to write to MCP server: {}", e))
        })?;
        conn.stdin.flush().await.map_err(|e| {
            McpTransportError::Connection(format!("Failed to flush MCP server stdin: {}", e))
        })?;

        let mut resp_line = String::new();
        let n = tokio::time::timeout(
            self.timeout,
            read_line_limited(&mut conn.stdout, &mut resp_line, MAX_MCP_RESPONSE_BYTES),
        )
        .await
        .map_err(|_| McpTransportError::Timeout(self.timeout))?
        .map_err(|e| McpTransportError::Connection(e.to_string()))?;

        if n == 0 {
            return Err(McpTransportError::Connection(
                "MCP server closed connection unexpectedly (server may have crashed)".into(),
            ));
        }

        let resp: JsonRpcResponse =
            serde_json::from_str(resp_line.trim()).map_err(|e| {
                McpTransportError::Connection(format!(
                    "Failed to parse MCP response: {} (raw: {:?})",
                    e,
                    resp_line.trim()
                ))
            })?;

        // If the response carries a JSON-RPC error, surface it as a Protocol error.
        if let Some(ref err) = resp.error {
            return Err(McpTransportError::Protocol {
                code: err.code,
                message: err.message.clone(),
            });
        }

        Ok(resp)
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        let mut child = self.child.lock().await;

        // Try SIGTERM first.
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let _ = child.id().map(|pid| {
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            });

            // Wait up to 5 seconds for graceful exit.
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(_) => return Ok(()),
                Err(_) => {
                    // Timed out — force kill.
                    let _ = child.kill().await;
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = child.kill().await;
        }

        Ok(())
    }

    fn transport_name(&self) -> &str {
        &self.name
    }
}
```

- [ ] **Step 2: Write tests for StdioTransport**

Since `StdioTransport` requires spawning a real subprocess, write an integration test using `echo` as a mock MCP server. Also test the `close()` path:

```rust
// Add to bottom of crates/agentos-mcp/src/transport/stdio.rs

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JsonRpcRequest;

    #[tokio::test]
    async fn spawn_nonexistent_command_returns_connection_error() {
        let result = StdioTransport::spawn(
            "test".into(),
            "nonexistent-binary-that-does-not-exist-xyz".into(),
            vec![],
            HashMap::new(),
            None,
            Some(5),
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.should_reconnect()); // Connection error
        assert!(err.to_string().contains("Failed to spawn"));
    }

    #[tokio::test]
    async fn transport_name_includes_server_name() {
        // We can't easily test send/close without a real MCP server,
        // but we can verify construction with a process that exits immediately.
        let transport = StdioTransport::spawn(
            "stdio:test-server".into(),
            "cat".into(),
            vec![],
            HashMap::new(),
            None,
            Some(2),
        )
        .await;
        // `cat` should spawn successfully
        if let Ok(t) = transport {
            assert_eq!(t.transport_name(), "stdio:test-server");
            let _ = t.close().await;
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p agentos-mcp`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/agentos-mcp/src/transport/stdio.rs
git commit -m "feat(mcp): add StdioTransport implementing McpTransport trait"
```

---

### Task 3: StreamableHttpTransport

**Files:**
- Modify: `crates/agentos-mcp/Cargo.toml`
- Create: `crates/agentos-mcp/src/transport/http.rs`

- [ ] **Step 1: Add reqwest dependency**

Add to `crates/agentos-mcp/Cargo.toml` under `[dependencies]`:

```toml
reqwest = { workspace = true }
```

- [ ] **Step 2: Write the StreamableHttpTransport**

```rust
// crates/agentos-mcp/src/transport/http.rs

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use super::McpTransportError;
use crate::types::{JsonRpcRequest, JsonRpcResponse};

/// Default timeout for HTTP requests.
const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

/// Streamable HTTP transport for MCP servers (spec revision 2025-03-26).
///
/// Sends JSON-RPC requests as HTTP POST to a single endpoint. Accepts either
/// a direct JSON response (`application/json`) or an SSE upgrade
/// (`text/event-stream`) for long-running calls.
pub struct StreamableHttpTransport {
    /// Display name for logging.
    name: String,
    /// The MCP server endpoint URL (e.g. "http://localhost:8080/mcp").
    url: String,
    /// Optional Bearer token for authentication.
    auth_token: Option<String>,
    /// Per-request timeout.
    timeout: Duration,
    /// Reqwest HTTP client (connection pooling built-in).
    client: reqwest::Client,
    /// Monotonically increasing JSON-RPC ID counter.
    next_id: AtomicU64,
}

impl StreamableHttpTransport {
    /// Create a new HTTP transport targeting the given URL.
    ///
    /// `auth_token` is the resolved plaintext Bearer token (the caller is
    /// responsible for vault lookup before constructing this transport).
    pub fn new(
        name: String,
        url: String,
        auth_token: Option<String>,
        timeout_secs: Option<u64>,
    ) -> Result<Self, McpTransportError> {
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_HTTP_TIMEOUT_SECS));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| {
                McpTransportError::Connection(format!("Failed to create HTTP client: {}", e))
            })?;

        Ok(Self {
            name,
            url,
            auth_token,
            timeout,
            client,
            next_id: AtomicU64::new(1),
        })
    }

    /// Parse an SSE stream to extract the JSON-RPC response.
    ///
    /// Reads `text/event-stream` lines until a `data:` line containing a valid
    /// JSON-RPC response is found. Per MCP Streamable HTTP spec, the response
    /// is sent as a single SSE `message` event.
    async fn parse_sse_response(&self, text: &str) -> Result<JsonRpcResponse, McpTransportError> {
        for line in text.lines() {
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(data) {
                    return Ok(resp);
                }
            }
        }
        Err(McpTransportError::Connection(
            "SSE stream ended without a valid JSON-RPC response".into(),
        ))
    }
}

#[async_trait]
impl super::McpTransport for StreamableHttpTransport {
    async fn send(&self, req: &JsonRpcRequest) -> Result<JsonRpcResponse, McpTransportError> {
        let mut request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");

        if let Some(ref token) = self.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let http_resp = request
            .json(req)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    McpTransportError::Timeout(self.timeout)
                } else if e.is_connect() {
                    McpTransportError::Connection(format!("HTTP connection refused: {}", e))
                } else {
                    McpTransportError::Connection(format!("HTTP request failed: {}", e))
                }
            })?;

        let status = http_resp.status();
        if !status.is_success() {
            let body = http_resp.text().await.unwrap_or_default();
            return Err(McpTransportError::Protocol {
                code: -(status.as_u16() as i64),
                message: format!("HTTP {}: {}", status, body),
            });
        }

        let content_type = http_resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let body = http_resp.text().await.map_err(|e| {
            McpTransportError::Connection(format!("Failed to read response body: {}", e))
        })?;

        if content_type.contains("text/event-stream") {
            // SSE response — parse the stream for the JSON-RPC response.
            let resp = self.parse_sse_response(&body).await?;
            if let Some(ref err) = resp.error {
                return Err(McpTransportError::Protocol {
                    code: err.code,
                    message: err.message.clone(),
                });
            }
            Ok(resp)
        } else {
            // Direct JSON response.
            let resp: JsonRpcResponse = serde_json::from_str(&body).map_err(|e| {
                McpTransportError::Connection(format!(
                    "Failed to parse JSON response: {} (raw: {:?})",
                    e,
                    &body[..body.len().min(200)]
                ))
            })?;
            if let Some(ref err) = resp.error {
                return Err(McpTransportError::Protocol {
                    code: err.code,
                    message: err.message.clone(),
                });
            }
            Ok(resp)
        }
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        // HTTP transport is stateless — nothing to close.
        // In-flight requests are cancelled when the client is dropped.
        Ok(())
    }

    fn transport_name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_transport_with_defaults() {
        let transport = StreamableHttpTransport::new(
            "http:test".into(),
            "http://localhost:9999/mcp".into(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(transport.transport_name(), "http:test");
        assert_eq!(transport.timeout, Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS));
    }

    #[test]
    fn new_with_custom_timeout() {
        let transport = StreamableHttpTransport::new(
            "http:test".into(),
            "http://localhost:9999/mcp".into(),
            Some("token123".into()),
            Some(60),
        )
        .unwrap();
        assert_eq!(transport.timeout, Duration::from_secs(60));
        assert_eq!(transport.auth_token.as_deref(), Some("token123"));
    }

    #[tokio::test]
    async fn parse_sse_response_valid() {
        let transport = StreamableHttpTransport::new(
            "test".into(),
            "http://localhost/mcp".into(),
            None,
            None,
        )
        .unwrap();

        let sse_body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let resp = transport.parse_sse_response(sse_body).await.unwrap();
        assert_eq!(resp.id, serde_json::json!(1));
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn parse_sse_response_no_data_returns_error() {
        let transport = StreamableHttpTransport::new(
            "test".into(),
            "http://localhost/mcp".into(),
            None,
            None,
        )
        .unwrap();

        let sse_body = "event: ping\n\n";
        let result = transport.parse_sse_response(sse_body).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn send_to_unreachable_server_returns_connection_error() {
        let transport = StreamableHttpTransport::new(
            "http:test".into(),
            // Use a port that's almost certainly not listening.
            "http://127.0.0.1:19999/mcp".into(),
            None,
            Some(2),
        )
        .unwrap();

        let req = JsonRpcRequest::new_no_params(1, "tools/list");
        let result = transport.send(&req).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().should_reconnect());
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p agentos-mcp`
Expected: All tests pass. The HTTP connection test will fail fast (connection refused to localhost:19999).

- [ ] **Step 4: Run clippy and fmt**

Run: `cargo clippy -p agentos-mcp -- -D warnings && cargo fmt --all -- --check`
Expected: Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-mcp/Cargo.toml
git add crates/agentos-mcp/src/transport/http.rs
git commit -m "feat(mcp): add StreamableHttpTransport for remote MCP servers"
```

---

### Task 4: Update client.rs to use shared util

**Files:**
- Modify: `crates/agentos-mcp/src/client.rs`

The old `McpClient` is still used by `McpServer` (server mode, out of scope). Update it to import from `transport::util` instead of duplicating the helper.

- [ ] **Step 1: Update imports in client.rs**

Replace the `read_line_limited` function and `MAX_MCP_RESPONSE_BYTES` constant in `client.rs` with imports from the new `transport::util` module:

Remove lines 14-57 (the constants and `read_line_limited` function) and replace with:

```rust
use crate::transport::util::{read_line_limited, MAX_MCP_RESPONSE_BYTES};
```

Also update `server.rs` which imports from `client`:

In `crates/agentos-mcp/src/server.rs`, change:
```rust
use crate::client::{read_line_limited, MAX_MCP_RESPONSE_BYTES};
```
to:
```rust
use crate::transport::util::{read_line_limited, MAX_MCP_RESPONSE_BYTES};
```

- [ ] **Step 2: Remove pub(crate) from client.rs helpers**

Since `read_line_limited` and `MAX_MCP_RESPONSE_BYTES` are no longer in `client.rs`, remove the `pub(crate)` visibility markers from the old definitions (they're deleted now).

- [ ] **Step 3: Run tests**

Run: `cargo test -p agentos-mcp`
Expected: All tests pass — server.rs tests still work with the new import path.

- [ ] **Step 4: Run full workspace build**

Run: `cargo build --workspace`
Expected: Clean build. No other crates import from `agentos-mcp::client` directly.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-mcp/src/client.rs
git add crates/agentos-mcp/src/server.rs
git commit -m "refactor(mcp): deduplicate read_line_limited into transport::util"
```

---

## Test Plan

| Test | Assertion |
|------|-----------|
| `McpTransportError::should_reconnect()` | `Connection` and `Timeout` return true, `Protocol` returns false |
| `McpTransportError::Display` | Each variant formats correctly |
| `read_line_limited` normal | Reads complete line within limit |
| `read_line_limited` over limit | Returns error mentioning byte limit |
| `read_line_limited` EOF | Returns 0 bytes |
| `StdioTransport::spawn` bad command | Returns `Connection` error |
| `StdioTransport::transport_name` | Returns configured name |
| `StreamableHttpTransport::new` defaults | Creates with 30s timeout |
| `StreamableHttpTransport::new` custom | Respects custom timeout and auth token |
| `StreamableHttpTransport::parse_sse_response` valid | Parses SSE data line into `JsonRpcResponse` |
| `StreamableHttpTransport::parse_sse_response` no data | Returns error |
| `StreamableHttpTransport::send` unreachable | Returns reconnectable `Connection` error |

## Verification

```bash
cargo test -p agentos-mcp
cargo build --workspace
cargo clippy -p agentos-mcp -- -D warnings
cargo fmt --all -- --check
```

All four commands must pass before this phase is complete.
