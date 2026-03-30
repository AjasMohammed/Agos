/// `McpServerHandle` — a resilient connection to a single MCP server process.
///
/// Unlike [`McpClient`] which is a raw connection, `McpServerHandle` adds:
///
/// * **Auto-reconnect** — when `call_tool` fails with a connection-level error
///   (server crash, broken pipe, timeout), the handle transparently re-spawns
///   the server process and retries the call **once**.
/// * **Health state** — `is_connected()` and `last_error()` expose the current
///   liveness of the server, suitable for `agentctl mcp status`.
/// * **Tool count** — tracks how many tools were registered from this server at
///   boot so the count remains available for health display after reconnects.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use tokio::sync::Mutex;

use crate::client::McpClient;
use crate::types::McpToolDef;

pub struct McpServerHandle {
    /// Human-readable server name from the config entry (used in logs / status).
    name: String,
    /// Executable to spawn (e.g. `"npx"`).
    command: String,
    /// Arguments passed to `command`.
    args: Vec<String>,
    /// Current live client, or `None` if the server is disconnected.
    ///
    /// The mutex is held only for brief operations — getting/swapping the
    /// reference.  It is **not** held during `call_tool` round-trips; those are
    /// serialized by the inner `McpClient::conn` mutex.
    client: Mutex<Option<Arc<McpClient>>>,
    /// Number of tools registered from this server at boot. Set once via
    /// [`McpServerHandle::set_tool_count`] after the registration loop.
    tool_count: AtomicUsize,
    /// Last connection-level error string, or `None` when connected cleanly.
    last_error: Mutex<Option<String>>,
}

impl McpServerHandle {
    /// Spawn the MCP server process, complete the initialize handshake, and
    /// return a shared handle.
    ///
    /// Returns `Arc<McpServerHandle>` so the handle can be shared with every
    /// [`McpToolAdapter`](crate::McpToolAdapter) created from this server's
    /// tool list.
    pub async fn spawn(
        name: String,
        command: String,
        args: Vec<String>,
    ) -> Result<Arc<Self>, anyhow::Error> {
        let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
        let client = McpClient::spawn_stdio(&command, &args_ref).await?;
        Ok(Arc::new(Self {
            name,
            command,
            args,
            client: Mutex::new(Some(client)),
            tool_count: AtomicUsize::new(0),
            last_error: Mutex::new(None),
        }))
    }

    // ── Public accessors ─────────────────────────────────────────────────────

    /// The server's configured name (from `config.mcp.servers[*].name`).
    pub fn server_name(&self) -> &str {
        &self.name
    }

    /// Number of tools registered from this server at boot.
    pub fn tool_count(&self) -> usize {
        self.tool_count.load(Ordering::Relaxed)
    }

    /// Record how many tools were registered. Called once after the boot-time
    /// registration loop to persist the count for health status display.
    pub fn set_tool_count(&self, n: usize) {
        self.tool_count.store(n, Ordering::Relaxed);
    }

    /// Whether the server process is currently believed to be alive.
    ///
    /// Returns `false` if the last call failed with a connection-level error
    /// and no reconnect has yet succeeded.
    pub async fn is_connected(&self) -> bool {
        self.client.lock().await.is_some()
    }

    /// Last connection-level error, if any.  `None` when the server is
    /// connected and the last call (if any) succeeded.
    pub async fn last_error(&self) -> Option<String> {
        self.last_error.lock().await.clone()
    }

    // ── Tool operations ──────────────────────────────────────────────────────

    /// List the tools advertised by this MCP server.
    ///
    /// Used at boot to discover what tools to register.  Returns an error
    /// immediately if the server is disconnected rather than reconnecting —
    /// callers at boot time should treat a disconnected server as a hard
    /// failure and skip registration.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, anyhow::Error> {
        let client = self
            .client
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' is not connected", self.name))?;
        client.list_tools().await
    }

    /// Call a tool on this MCP server.
    ///
    /// On a connection-level error (crash, broken pipe, timeout), the handle
    /// automatically tries to re-spawn the server and retries the call once.
    /// A protocol-level error (JSON-RPC error response from a live server)
    /// is returned directly without triggering a reconnect.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, anyhow::Error> {
        // Get or restore the live client before the call.
        let client = match self.try_get_or_reconnect().await {
            Ok(c) => c,
            Err(e) => return Err(e),
        };

        match client.call_tool(tool_name, arguments.clone()).await {
            Ok(result) => {
                // Clear any stale last_error on success.
                *self.last_error.lock().await = None;
                Ok(result)
            }
            Err(e) if is_connection_error(&e) => {
                // Server appears to have died.  Clear the client reference and
                // attempt one reconnect + retry.
                tracing::warn!(
                    server = %self.name,
                    tool = %tool_name,
                    error = %e,
                    "MCP server connection lost — attempting reconnect and retry"
                );
                *self.client.lock().await = None;
                *self.last_error.lock().await = Some(e.to_string());

                match self.reconnect().await {
                    Ok(new_client) => match new_client.call_tool(tool_name, arguments).await {
                        Ok(result) => {
                            *self.last_error.lock().await = None;
                            Ok(result)
                        }
                        Err(e2) => {
                            *self.last_error.lock().await = Some(e2.to_string());
                            Err(e2)
                        }
                    },
                    Err(e2) => {
                        let msg = format!("MCP server '{}' reconnect failed: {}", self.name, e2);
                        *self.last_error.lock().await = Some(msg.clone());
                        anyhow::bail!("{}", msg)
                    }
                }
            }
            Err(e) => {
                // Protocol-level error (e.g. JSON-RPC `-32603`) — the server
                // process is still alive, no reconnect needed.
                Err(e)
            }
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Return the current client if connected, or attempt a reconnect.
    async fn try_get_or_reconnect(&self) -> Result<Arc<McpClient>, anyhow::Error> {
        match self.client.lock().await.clone() {
            Some(c) => Ok(c),
            None => self.reconnect().await,
        }
    }

    /// Re-spawn the server subprocess and store the new client.
    async fn reconnect(&self) -> Result<Arc<McpClient>, anyhow::Error> {
        let args_ref: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let new_client = McpClient::spawn_stdio(&self.command, &args_ref)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to reconnect to MCP server '{}': {}", self.name, e)
            })?;
        *self.client.lock().await = Some(Arc::clone(&new_client));
        tracing::info!(server = %self.name, "MCP server reconnected successfully");
        Ok(new_client)
    }
}

/// Returns `true` if `e` is a connection-level failure that warrants a
/// reconnect attempt rather than a protocol-level error that the server sent
/// intentionally.
fn is_connection_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("closed connection")
        || msg.contains("did not respond")
        || msg.contains("Failed to spawn")
        || msg.contains("broken pipe")
        || msg.contains("BrokenPipe")
}
