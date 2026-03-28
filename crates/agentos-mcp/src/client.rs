/// MCP client — spawns an MCP server subprocess and communicates via
/// newline-delimited JSON over stdio (the standard MCP transport).
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::types::{JsonRpcRequest, JsonRpcResponse, McpToolDef};

// ── Limits ────────────────────────────────────────────────────────────────────

/// Maximum time to wait for a single MCP request/response round-trip.
/// A server that does not respond within this window is considered hung.
const MCP_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Maximum number of bytes accepted from a single MCP server response line.
/// Prevents memory exhaustion from a malicious or malfunctioning server.
pub(crate) const MAX_MCP_RESPONSE_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// Read a single newline-terminated line from `reader`, enforcing a byte limit
/// *during* the read rather than after. This prevents a malicious server from
/// exhausting memory by sending a very large payload without a newline.
///
/// Returns the number of bytes read (0 means EOF).
pub(crate) async fn read_line_limited(
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

// ── Connection guard ──────────────────────────────────────────────────────────

/// Holds the stdin/stdout pair for an MCP subprocess connection.
///
/// Kept inside a single `Mutex` so that a write and the subsequent read for
/// the same request are never interleaved with another concurrent caller.
struct McpConnection {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

// ── McpClient ─────────────────────────────────────────────────────────────────

/// A live connection to an MCP server process.
///
/// The child process is kept alive for the lifetime of this struct and is
/// automatically killed (via `kill_on_drop`) when the struct is dropped.
///
/// # Concurrency
///
/// All stdin/stdout I/O is protected by a **single** `Mutex<McpConnection>`.
/// This means the write-then-read of a request/response round-trip is always
/// atomic — no other caller can inject a write between our write and read.
/// The JSON-RPC ID counter is atomic and never reused within a session.
pub struct McpClient {
    /// Single connection guard — write and read held together to prevent
    /// request/response interleaving across concurrent callers.
    conn: Mutex<McpConnection>,
    /// Kept alive so the child process lives as long as the client does.
    /// `kill_on_drop(true)` ensures the subprocess is cleaned up on drop.
    _child: Child,
    next_id: AtomicU64,
}

impl McpClient {
    /// Spawn an MCP server process and perform the `initialize` handshake.
    ///
    /// Returns `Arc<McpClient>` so the connection can be shared across tasks.
    ///
    /// The child process inherits only a minimal set of environment variables
    /// (`PATH`, `HOME`, `TMPDIR`) — the kernel's API keys and other secrets
    /// are never leaked into MCP subprocess environments.
    ///
    /// # Errors
    ///
    /// Returns an error if the process fails to spawn, if the stdio pipes
    /// cannot be acquired, if the handshake times out, or if the server
    /// returns an error response to `initialize`.
    pub async fn spawn_stdio(command: &str, args: &[&str]) -> Result<Arc<Self>, anyhow::Error> {
        // Collect the safe subset of the current environment to pass through.
        let safe_env: Vec<(String, String)> = ["PATH", "HOME", "TMPDIR", "TEMP", "TMP"]
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();

        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // Suppress MCP server stderr so it doesn't pollute our own stderr.
            .stderr(std::process::Stdio::null())
            // Do NOT inherit the kernel's full environment (API keys, secrets, etc.)
            // into the MCP subprocess. Only pass a safe minimal subset.
            .env_clear()
            .envs(safe_env)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn MCP server '{}': {}", command, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to acquire stdin pipe for '{}'", command))?;
        let stdout =
            BufReader::new(child.stdout.take().ok_or_else(|| {
                anyhow::anyhow!("Failed to acquire stdout pipe for '{}'", command)
            })?);

        let client = Arc::new(Self {
            conn: Mutex::new(McpConnection { stdin, stdout }),
            _child: child,
            next_id: AtomicU64::new(1),
        });

        client.initialize().await?;
        Ok(client)
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Send a single JSON-RPC request and await the response.
    ///
    /// The connection mutex is held for the **entire** round-trip (write +
    /// read), ensuring that concurrent callers cannot interleave their
    /// request writes and response reads.
    ///
    /// Returns an error if the server does not respond within
    /// [`MCP_REQUEST_TIMEOUT_SECS`], if the server closes the connection, or
    /// if the response exceeds [`MAX_MCP_RESPONSE_BYTES`].
    async fn send(&self, req: &JsonRpcRequest) -> Result<JsonRpcResponse, anyhow::Error> {
        let mut conn = self.conn.lock().await;

        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        conn.stdin.write_all(line.as_bytes()).await?;
        conn.stdin.flush().await?;

        let mut resp_line = String::new();
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(MCP_REQUEST_TIMEOUT_SECS),
            read_line_limited(&mut conn.stdout, &mut resp_line, MAX_MCP_RESPONSE_BYTES),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "MCP server did not respond within {}s",
                MCP_REQUEST_TIMEOUT_SECS
            )
        })??;

        if n == 0 {
            anyhow::bail!("MCP server closed connection unexpectedly (server may have crashed)");
        }

        serde_json::from_str(resp_line.trim()).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse MCP response: {} (raw: {:?})",
                e,
                resp_line.trim()
            )
        })
    }

    /// Perform the MCP `initialize` + `notifications/initialized` handshake.
    ///
    /// Both the request/response and the follow-up notification are sent while
    /// holding the connection mutex, so they are guaranteed to be contiguous
    /// with no interleaving from other callers.
    async fn initialize(&self) -> Result<(), anyhow::Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(
            id,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "agentos", "version": env!("CARGO_PKG_VERSION") }
            }),
        );

        // Hold the connection lock for the entire initialize exchange so that
        // the request, response, and follow-up notification form an unbroken
        // sequence on the wire.
        let mut conn = self.conn.lock().await;

        // Write initialize request.
        let mut req_line = serde_json::to_string(&req)?;
        req_line.push('\n');
        conn.stdin.write_all(req_line.as_bytes()).await?;
        conn.stdin.flush().await?;

        // Read initialize response (with timeout and bounded read).
        let mut resp_line = String::new();
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(MCP_REQUEST_TIMEOUT_SECS),
            read_line_limited(&mut conn.stdout, &mut resp_line, MAX_MCP_RESPONSE_BYTES),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "MCP initialize timed out after {}s",
                MCP_REQUEST_TIMEOUT_SECS
            )
        })??;

        if n == 0 {
            anyhow::bail!("MCP server closed connection during initialize handshake");
        }

        let resp: JsonRpcResponse = serde_json::from_str(resp_line.trim()).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse MCP initialize response: {} (raw: {:?})",
                e,
                resp_line.trim()
            )
        })?;
        if let Some(err) = resp.error {
            anyhow::bail!("MCP initialize failed (code {}): {}", err.code, err.message);
        }

        // Send the `notifications/initialized` notification (fire-and-forget;
        // the server does not send a response for notifications).
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let mut notif_line = serde_json::to_string(&notif)?;
        notif_line.push('\n');
        conn.stdin.write_all(notif_line.as_bytes()).await?;
        conn.stdin.flush().await?;

        Ok(())
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// List all tools advertised by this MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, anyhow::Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new_no_params(id, "tools/list");
        let resp = self.send(&req).await?;

        if let Some(err) = resp.error {
            anyhow::bail!("tools/list error (code {}): {}", err.code, err.message);
        }

        let result = resp.result.unwrap_or(serde_json::Value::Null);
        let tools: Vec<McpToolDef> = serde_json::from_value(
            result
                .get("tools")
                .cloned()
                .unwrap_or(serde_json::Value::Array(vec![])),
        )?;
        Ok(tools)
    }

    /// Call a tool on the MCP server and return the raw result value.
    ///
    /// Permission validation has already been performed by the AgentOS
    /// `ToolRunner` before this call is reached.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, anyhow::Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(
            id,
            "tools/call",
            serde_json::json!({
                "name": tool_name,
                "arguments": arguments,
            }),
        );

        let resp = self.send(&req).await?;
        if let Some(err) = resp.error {
            anyhow::bail!(
                "tools/call '{}' error (code {}): {}",
                tool_name,
                err.code,
                err.message
            );
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}
