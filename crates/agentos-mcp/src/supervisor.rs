use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use crate::transport::{McpTransport, McpTransportFactory};
use crate::types::{JsonRpcRequest, McpLifecycleEvent, McpToolDef, ServerState, ServerStats};

/// Configuration for a single MCP server, resolved and ready for the supervisor.
///
/// This is the supervisor's view of the config — all vault references resolved,
/// transport type decided. Constructed by the kernel from `McpServerConfig`.
#[derive(Debug, Clone)]
pub struct McpServerResolvedConfig {
    pub name: String,
    pub timeout_secs: u64,
    pub auto_reconnect: bool,
    pub health_check_interval_secs: u64,
}

/// A single supervised MCP server connection.
pub struct SupervisedServer {
    pub config: McpServerResolvedConfig,
    pub transport: Arc<dyn McpTransport>,
    /// Factory for creating fresh transports on reconnect (stdio needs new process).
    pub transport_factory: Option<Arc<dyn McpTransportFactory>>,
    pub state: ServerState,
    pub tools: Vec<McpToolDef>,
    pub stats: ServerStats,
    /// Current backoff delay. Doubles on each failure, caps at MAX_BACKOFF.
    backoff_delay: Duration,
    /// Number of consecutive reconnect attempts.
    reconnect_attempts: u32,
}

/// Maximum backoff delay between reconnection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(300); // 5 minutes
/// Initial backoff delay.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Default health check interval.
const DEFAULT_HEALTH_INTERVAL_SECS: u64 = 30;

impl SupervisedServer {
    pub fn new(config: McpServerResolvedConfig, transport: Arc<dyn McpTransport>) -> Self {
        Self {
            config,
            transport,
            transport_factory: None,
            state: ServerState::Connecting,
            tools: Vec::new(),
            stats: ServerStats::default(),
            backoff_delay: INITIAL_BACKOFF,
            reconnect_attempts: 0,
        }
    }

    /// Calculate the next backoff delay with jitter.
    pub fn next_backoff(&mut self) -> Duration {
        let delay = self.backoff_delay;
        // Double the delay for next time, capped at MAX_BACKOFF.
        self.backoff_delay = (self.backoff_delay * 2).min(MAX_BACKOFF);
        self.reconnect_attempts += 1;
        // Add jitter: +/- 25% of the delay.
        let jitter_range = delay.as_millis() as f64 * 0.25;
        let jitter = (rand_jitter() * 2.0 - 1.0) * jitter_range;
        Duration::from_millis((delay.as_millis() as f64 + jitter).max(100.0) as u64)
    }

    /// Reset backoff state after a successful connection.
    pub fn reset_backoff(&mut self) {
        self.backoff_delay = INITIAL_BACKOFF;
        self.reconnect_attempts = 0;
        self.stats.reset_failures();
    }
}

/// Simple pseudo-random jitter in [0, 1) range.
/// Uses the current time nanoseconds as entropy — good enough for backoff jitter.
fn rand_jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0
}

/// Manages all MCP server connections with health monitoring and lifecycle events.
pub struct McpSupervisor {
    servers: Arc<RwLock<HashMap<String, SupervisedServer>>>,
    event_tx: mpsc::Sender<McpLifecycleEvent>,
    cancellation_token: CancellationToken,
    next_id: AtomicU64,
}

impl McpSupervisor {
    /// Create a new supervisor.
    ///
    /// `event_tx` is used to send lifecycle events to the kernel for audit logging.
    /// `cancellation_token` controls the health loop shutdown.
    pub fn new(
        event_tx: mpsc::Sender<McpLifecycleEvent>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            cancellation_token,
            next_id: AtomicU64::new(100), // Reserve low IDs for init/tools/list
        }
    }

    /// Perform the MCP initialize handshake on a transport.
    ///
    /// Sends `initialize` request, validates the response, then sends
    /// `notifications/initialized`. Returns the protocol version on success.
    pub async fn initialize_transport(
        transport: &dyn McpTransport,
    ) -> Result<String, crate::transport::McpTransportError> {
        let init_req = JsonRpcRequest::new(
            0,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "agentos", "version": env!("CARGO_PKG_VERSION") }
            }),
        );

        let resp = transport.send(&init_req).await?;

        let version = resp
            .result
            .as_ref()
            .and_then(|r| r.get("protocolVersion"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Send the initialized notification (fire-and-forget).
        let notif = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: serde_json::Value::Null,
            method: "notifications/initialized".to_string(),
            params: None,
        };
        // Intentionally ignore errors — it's a notification.
        let _ = transport.send_notification(&notif).await;

        Ok(version)
    }

    /// Discover tools from an MCP server via `tools/list`.
    pub async fn list_tools_from_transport(
        transport: &dyn McpTransport,
    ) -> Result<Vec<McpToolDef>, crate::transport::McpTransportError> {
        let req = JsonRpcRequest::new_no_params(1, "tools/list");
        let resp = transport.send(&req).await?;

        let tools: Vec<McpToolDef> = resp
            .result
            .and_then(|r| r.get("tools").cloned())
            .and_then(|t| match serde_json::from_value::<Vec<McpToolDef>>(t) {
                Ok(tools) => Some(tools),
                Err(e) => {
                    tracing::warn!(
                        transport = %transport.transport_name(),
                        error = %e,
                        "Failed to deserialize MCP tools/list response"
                    );
                    None
                }
            })
            .unwrap_or_default();

        Ok(tools)
    }

    /// Add a server to the supervisor, perform handshake, and discover tools.
    ///
    /// On success, the server enters `Connected` state and its tools are returned.
    /// On failure, the server enters `Backoff` state if `auto_reconnect` is true,
    /// or is not added at all if `auto_reconnect` is false.
    pub async fn add_server(
        &self,
        config: McpServerResolvedConfig,
        transport: Arc<dyn McpTransport>,
    ) -> Result<Vec<McpToolDef>, crate::transport::McpTransportError> {
        self.add_server_with_factory(config, transport, None).await
    }

    /// Add a server with a transport factory for reconnection support.
    ///
    /// The factory is used to create fresh transports when the supervisor
    /// needs to reconnect (e.g., stdio transport after a process crash).
    pub async fn add_server_with_factory(
        &self,
        config: McpServerResolvedConfig,
        transport: Arc<dyn McpTransport>,
        factory: Option<Arc<dyn McpTransportFactory>>,
    ) -> Result<Vec<McpToolDef>, crate::transport::McpTransportError> {
        let name = config.name.clone();
        let auto_reconnect = config.auto_reconnect;

        let mut server = SupervisedServer::new(config, Arc::clone(&transport));
        server.transport_factory = factory;

        match Self::initialize_transport(transport.as_ref()).await {
            Ok(_version) => match Self::list_tools_from_transport(transport.as_ref()).await {
                Ok(tools) => {
                    server.state = ServerState::Connected;
                    server.tools = tools.clone();
                    server.reset_backoff();

                    let _ = self
                        .event_tx
                        .send(McpLifecycleEvent::ServerConnected {
                            name: name.clone(),
                            tool_count: tools.len(),
                        })
                        .await;

                    self.servers.write().await.insert(name, server);
                    Ok(tools)
                }
                Err(e) => {
                    if auto_reconnect {
                        server.state = ServerState::Backoff;
                        server.stats.record_failure();
                        let _ = self
                            .event_tx
                            .send(McpLifecycleEvent::ServerDisconnected {
                                name: name.clone(),
                                error: e.to_string(),
                            })
                            .await;
                        self.servers.write().await.insert(name, server);
                    }
                    Err(e)
                }
            },
            Err(e) => {
                if auto_reconnect {
                    server.state = ServerState::Backoff;
                    server.stats.record_failure();
                    let _ = self
                        .event_tx
                        .send(McpLifecycleEvent::ServerDisconnected {
                            name: name.clone(),
                            error: e.to_string(),
                        })
                        .await;
                    self.servers.write().await.insert(name, server);
                }
                Err(e)
            }
        }
    }

    /// Remove a server by name. Closes the transport and emits a `ServerStopped` event.
    pub async fn remove_server(&self, name: &str) -> bool {
        let mut servers = self.servers.write().await;
        if let Some(mut server) = servers.remove(name) {
            server.state = ServerState::Stopped;
            let _ = server.transport.close().await;
            let _ = self
                .event_tx
                .send(McpLifecycleEvent::ServerStopped {
                    name: name.to_string(),
                })
                .await;
            true
        } else {
            false
        }
    }

    /// Call a tool on a specific server.
    ///
    /// Returns the raw JSON result on success. On a connection error,
    /// transitions the server to `Backoff` state.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, crate::transport::McpTransportError> {
        let transport = {
            let servers = self.servers.read().await;
            let server = servers.get(server_name).ok_or_else(|| {
                crate::transport::McpTransportError::Connection(format!(
                    "MCP server '{}' not found",
                    server_name
                ))
            })?;
            if server.state != ServerState::Connected {
                return Err(crate::transport::McpTransportError::Connection(format!(
                    "MCP server '{}' is in state '{}', not connected",
                    server_name, server.state
                )));
            }
            Arc::clone(&server.transport)
        };

        let start = tokio::time::Instant::now();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(
            id,
            "tools/call",
            serde_json::json!({
                "name": tool_name,
                "arguments": arguments,
            }),
        );

        match transport.send(&req).await {
            Ok(resp) => {
                let latency_ms = start.elapsed().as_millis() as u64;
                {
                    let mut servers = self.servers.write().await;
                    if let Some(server) = servers.get_mut(server_name) {
                        server.stats.record_call(latency_ms);
                    }
                }
                let _ = self
                    .event_tx
                    .send(McpLifecycleEvent::ToolCallCompleted {
                        server: server_name.to_string(),
                        tool: tool_name.to_string(),
                        latency_ms,
                        success: true,
                    })
                    .await;
                Ok(resp.result.unwrap_or(serde_json::Value::Null))
            }
            Err(e) if e.should_reconnect() => {
                let latency_ms = start.elapsed().as_millis() as u64;
                {
                    let mut servers = self.servers.write().await;
                    if let Some(server) = servers.get_mut(server_name) {
                        server.state = ServerState::Backoff;
                        server.stats.record_failure();
                    }
                }
                let _ = self
                    .event_tx
                    .send(McpLifecycleEvent::ServerDisconnected {
                        name: server_name.to_string(),
                        error: e.to_string(),
                    })
                    .await;
                let _ = self
                    .event_tx
                    .send(McpLifecycleEvent::ToolCallCompleted {
                        server: server_name.to_string(),
                        tool: tool_name.to_string(),
                        latency_ms,
                        success: false,
                    })
                    .await;
                Err(e)
            }
            Err(e) => {
                let latency_ms = start.elapsed().as_millis() as u64;
                let _ = self
                    .event_tx
                    .send(McpLifecycleEvent::ToolCallCompleted {
                        server: server_name.to_string(),
                        tool: tool_name.to_string(),
                        latency_ms,
                        success: false,
                    })
                    .await;
                Err(e)
            }
        }
    }

    /// Get the current state and stats for all servers.
    pub async fn server_statuses(
        &self,
    ) -> Vec<(String, ServerState, usize, ServerStats, Option<String>)> {
        let servers = self.servers.read().await;
        servers
            .iter()
            .map(|(name, server)| {
                (
                    name.clone(),
                    server.state,
                    server.tools.len(),
                    server.stats.clone(),
                    if server.state == ServerState::Backoff {
                        Some(format!("reconnect attempt {}", server.reconnect_attempts))
                    } else {
                        None
                    },
                )
            })
            .collect()
    }

    /// Get the cached tool list for a specific server.
    pub async fn server_tools(&self, server_name: &str) -> Option<Vec<McpToolDef>> {
        let servers = self.servers.read().await;
        servers.get(server_name).map(|s| s.tools.clone())
    }

    /// Gracefully shut down all servers.
    pub async fn shutdown(&self) {
        let mut servers = self.servers.write().await;
        for (name, server) in servers.iter_mut() {
            server.state = ServerState::Stopped;
            let _ = server.transport.close().await;
            let _ = self
                .event_tx
                .send(McpLifecycleEvent::ServerStopped { name: name.clone() })
                .await;
        }
        servers.clear();
    }

    /// Spawn the background health check loop.
    ///
    /// The loop runs until the `CancellationToken` is cancelled. On each tick:
    /// - For `Connected` servers: sends `tools/list` as a health ping.
    ///   On failure, transitions to `Backoff`.
    /// - For `Backoff` servers: attempts reconnect (initialize + tools/list).
    ///   On success, transitions to `Connected` and emits `ServerConnected`.
    pub fn spawn_health_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let supervisor = Arc::clone(self);
        let token = self.cancellation_token.clone();

        tokio::spawn(async move {
            let interval = Duration::from_secs(DEFAULT_HEALTH_INTERVAL_SECS);
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        tracing::info!("MCP health loop shutting down");
                        break;
                    }
                    _ = tick.tick() => {
                        supervisor.run_health_check().await;
                    }
                }
            }
        })
    }

    /// Run a single health check pass over all servers.
    async fn run_health_check(&self) {
        let server_names: Vec<(String, ServerState)> = {
            let servers = self.servers.read().await;
            servers
                .iter()
                .filter(|(_, s)| s.state != ServerState::Stopped)
                .map(|(name, s)| (name.clone(), s.state))
                .collect()
        };

        for (name, state) in server_names {
            match state {
                ServerState::Connected => {
                    self.health_check_connected(&name).await;
                }
                ServerState::Backoff => {
                    self.try_reconnect(&name).await;
                }
                _ => {} // Connecting and Stopped are handled elsewhere
            }
        }
    }

    /// Health-check a connected server by sending `tools/list`.
    async fn health_check_connected(&self, name: &str) {
        let transport = {
            let servers = self.servers.read().await;
            match servers.get(name) {
                Some(s) if s.state == ServerState::Connected => Arc::clone(&s.transport),
                _ => return,
            }
        };

        match Self::list_tools_from_transport(transport.as_ref()).await {
            Ok(tools) => {
                // Update cached tool list if it changed.
                let mut servers = self.servers.write().await;
                if let Some(server) = servers.get_mut(name) {
                    if server.tools.len() != tools.len()
                        || server
                            .tools
                            .iter()
                            .zip(tools.iter())
                            .any(|(a, b)| a.name != b.name)
                    {
                        tracing::info!(
                            server = %name,
                            old_count = server.tools.len(),
                            new_count = tools.len(),
                            "MCP server tool list changed"
                        );
                        server.tools = tools;
                    }
                }
            }
            Err(e) if e.should_reconnect() => {
                tracing::warn!(
                    server = %name,
                    error = %e,
                    "MCP server health check failed — transitioning to backoff"
                );
                let mut servers = self.servers.write().await;
                if let Some(server) = servers.get_mut(name) {
                    server.state = ServerState::Backoff;
                    server.stats.record_failure();
                }
                let _ = self
                    .event_tx
                    .send(McpLifecycleEvent::ServerDisconnected {
                        name: name.to_string(),
                        error: e.to_string(),
                    })
                    .await;
            }
            Err(_) => {
                // Protocol error — server is alive but returned an error.
                // Don't transition to backoff.
            }
        }
    }

    /// Attempt to reconnect a server in Backoff state.
    ///
    /// If a transport factory is available, creates a fresh transport
    /// (needed for stdio where the old process is dead). Otherwise
    /// reuses the existing transport (works for HTTP which is stateless).
    async fn try_reconnect(&self, name: &str) {
        // Phase 1: mark as Connecting and extract factory (if any).
        let (factory, attempt) = {
            let mut servers = self.servers.write().await;
            let server = match servers.get_mut(name) {
                Some(s) if s.state == ServerState::Backoff => s,
                _ => return,
            };
            server.state = ServerState::Connecting;
            let attempt = server.reconnect_attempts;
            (server.transport_factory.clone(), attempt)
        };
        // Lock released — factory.create() can now run without blocking other operations.

        // Phase 2: create fresh transport outside the lock (may spawn subprocess).
        if let Some(factory) = &factory {
            match factory.create().await {
                Ok(new_transport) => {
                    let mut servers = self.servers.write().await;
                    if let Some(server) = servers.get_mut(name) {
                        server.transport = new_transport;
                    }
                }
                Err(e) => {
                    let mut servers = self.servers.write().await;
                    if let Some(server) = servers.get_mut(name) {
                        let _delay = server.next_backoff();
                        server.state = ServerState::Backoff;
                    }
                    tracing::warn!(server = %name, error = %e, "MCP reconnect: transport factory failed");
                    return;
                }
            }
        }

        // Phase 3: get the (possibly refreshed) transport.
        let transport = {
            let servers = self.servers.read().await;
            match servers.get(name) {
                Some(s) => Arc::clone(&s.transport),
                None => return,
            }
        };

        let _ = self
            .event_tx
            .send(McpLifecycleEvent::ServerReconnecting {
                name: name.to_string(),
                attempt,
            })
            .await;

        match Self::initialize_transport(transport.as_ref()).await {
            Ok(_) => match Self::list_tools_from_transport(transport.as_ref()).await {
                Ok(tools) => {
                    let mut servers = self.servers.write().await;
                    if let Some(server) = servers.get_mut(name) {
                        server.state = ServerState::Connected;
                        server.tools = tools.clone();
                        server.reset_backoff();
                    }
                    let _ = self
                        .event_tx
                        .send(McpLifecycleEvent::ServerConnected {
                            name: name.to_string(),
                            tool_count: tools.len(),
                        })
                        .await;
                    tracing::info!(server = %name, tools = tools.len(), "MCP server reconnected");
                }
                Err(e) => {
                    let mut servers = self.servers.write().await;
                    if let Some(server) = servers.get_mut(name) {
                        let _delay = server.next_backoff();
                        server.state = ServerState::Backoff;
                    }
                    tracing::warn!(server = %name, error = %e, "MCP reconnect: tools/list failed");
                }
            },
            Err(e) => {
                let mut servers = self.servers.write().await;
                if let Some(server) = servers.get_mut(name) {
                    let _delay = server.next_backoff();
                    server.state = ServerState::Backoff;
                }
                tracing::warn!(server = %name, error = %e, "MCP reconnect: initialize failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::McpTransportError;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// A mock transport that can be configured to succeed or fail.
    struct MockTransport {
        name: String,
        should_fail: AtomicBool,
        call_count: AtomicU32,
        tools: Vec<McpToolDef>,
    }

    impl MockTransport {
        fn new(name: &str, tools: Vec<McpToolDef>) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                should_fail: AtomicBool::new(false),
                call_count: AtomicU32::new(0),
                tools,
            })
        }

        fn set_fail(&self, fail: bool) {
            self.should_fail.store(fail, Ordering::Relaxed);
        }
    }

    #[async_trait::async_trait]
    impl McpTransport for MockTransport {
        async fn send(
            &self,
            req: &JsonRpcRequest,
        ) -> Result<crate::types::JsonRpcResponse, McpTransportError> {
            self.call_count.fetch_add(1, Ordering::Relaxed);

            if self.should_fail.load(Ordering::Relaxed) {
                return Err(McpTransportError::Connection(
                    "mock connection failure".into(),
                ));
            }

            match req.method.as_str() {
                "initialize" => Ok(crate::types::JsonRpcResponse::ok(
                    req.id.clone(),
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "serverInfo": { "name": "mock", "version": "1.0" }
                    }),
                )),
                "notifications/initialized" => Ok(crate::types::JsonRpcResponse::ok(
                    req.id.clone(),
                    serde_json::json!({}),
                )),
                "tools/list" => Ok(crate::types::JsonRpcResponse::ok(
                    req.id.clone(),
                    serde_json::json!({ "tools": self.tools }),
                )),
                "tools/call" => Ok(crate::types::JsonRpcResponse::ok(
                    req.id.clone(),
                    serde_json::json!({
                        "content": [{ "type": "text", "text": "mock result" }]
                    }),
                )),
                _ => Err(McpTransportError::Protocol {
                    code: -32601,
                    message: "method not found".into(),
                }),
            }
        }

        async fn send_notification(&self, _req: &JsonRpcRequest) -> Result<(), McpTransportError> {
            if self.should_fail.load(Ordering::Relaxed) {
                return Err(McpTransportError::Connection(
                    "mock connection failure".into(),
                ));
            }
            Ok(())
        }

        async fn close(&self) -> Result<(), McpTransportError> {
            Ok(())
        }

        fn transport_name(&self) -> &str {
            &self.name
        }
    }

    fn test_config(name: &str) -> McpServerResolvedConfig {
        McpServerResolvedConfig {
            name: name.to_string(),
            timeout_secs: 30,
            auto_reconnect: true,
            health_check_interval_secs: 30,
        }
    }

    fn mock_tool(name: &str) -> McpToolDef {
        McpToolDef {
            name: name.to_string(),
            description: format!("Mock tool {}", name),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[tokio::test]
    async fn add_server_success() {
        let (tx, mut rx) = mpsc::channel(10);
        let supervisor = McpSupervisor::new(tx, CancellationToken::new());

        let transport = MockTransport::new("stdio:test", vec![mock_tool("ping")]);
        let tools = supervisor
            .add_server(test_config("test"), transport)
            .await
            .unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "ping");

        // Should have emitted ServerConnected event.
        let event = rx.recv().await.unwrap();
        assert!(matches!(
            event,
            McpLifecycleEvent::ServerConnected {
                ref name,
                tool_count
            } if name == "test" && tool_count == 1
        ));

        // Server should be in Connected state.
        let statuses = supervisor.server_statuses().await;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].1, ServerState::Connected);
    }

    #[tokio::test]
    async fn add_server_failure_enters_backoff() {
        let (tx, mut rx) = mpsc::channel(10);
        let supervisor = McpSupervisor::new(tx, CancellationToken::new());

        let transport = MockTransport::new("stdio:test", vec![]);
        transport.set_fail(true);

        let result = supervisor.add_server(test_config("test"), transport).await;

        assert!(result.is_err());

        // Server should be in Backoff state (auto_reconnect = true).
        let statuses = supervisor.server_statuses().await;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].1, ServerState::Backoff);

        // Should have emitted ServerDisconnected.
        let event = rx.recv().await.unwrap();
        assert!(matches!(
            event,
            McpLifecycleEvent::ServerDisconnected { .. }
        ));
    }

    #[tokio::test]
    async fn call_tool_success() {
        let (tx, mut rx) = mpsc::channel(10);
        let supervisor = McpSupervisor::new(tx, CancellationToken::new());

        let transport = MockTransport::new("stdio:test", vec![mock_tool("ping")]);
        supervisor
            .add_server(test_config("test"), transport)
            .await
            .unwrap();
        // Drain the ServerConnected event.
        let _ = rx.recv().await;

        let result = supervisor
            .call_tool("test", "ping", serde_json::json!({}))
            .await
            .unwrap();

        assert!(result.is_object());

        // Should have emitted ToolCallCompleted.
        let event = rx.recv().await.unwrap();
        assert!(matches!(
            event,
            McpLifecycleEvent::ToolCallCompleted { success: true, .. }
        ));
    }

    #[tokio::test]
    async fn call_tool_connection_error_transitions_to_backoff() {
        let (tx, mut rx) = mpsc::channel(10);
        let supervisor = McpSupervisor::new(tx, CancellationToken::new());

        let transport = MockTransport::new("stdio:test", vec![mock_tool("ping")]);
        supervisor
            .add_server(
                test_config("test"),
                Arc::clone(&transport) as Arc<dyn McpTransport>,
            )
            .await
            .unwrap();
        let _ = rx.recv().await; // drain ServerConnected

        // Now make the transport fail.
        transport.set_fail(true);

        let result = supervisor
            .call_tool("test", "ping", serde_json::json!({}))
            .await;
        assert!(result.is_err());

        // Server should be in Backoff state.
        let statuses = supervisor.server_statuses().await;
        assert_eq!(statuses[0].1, ServerState::Backoff);
    }

    #[tokio::test]
    async fn remove_server_emits_stopped() {
        let (tx, mut rx) = mpsc::channel(10);
        let supervisor = McpSupervisor::new(tx, CancellationToken::new());

        let transport = MockTransport::new("stdio:test", vec![mock_tool("ping")]);
        supervisor
            .add_server(test_config("test"), transport)
            .await
            .unwrap();
        let _ = rx.recv().await; // drain ServerConnected

        assert!(supervisor.remove_server("test").await);
        assert!(!supervisor.remove_server("nonexistent").await);

        let statuses = supervisor.server_statuses().await;
        assert!(statuses.is_empty());

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, McpLifecycleEvent::ServerStopped { ref name } if name == "test"));
    }

    #[tokio::test]
    async fn call_tool_unknown_server_returns_error() {
        let (tx, _rx) = mpsc::channel(10);
        let supervisor = McpSupervisor::new(tx, CancellationToken::new());

        let result = supervisor
            .call_tool("nonexistent", "ping", serde_json::json!({}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shutdown_stops_all_servers() {
        let (tx, _rx) = mpsc::channel(10);
        let supervisor = McpSupervisor::new(tx, CancellationToken::new());

        let t1 = MockTransport::new("stdio:s1", vec![mock_tool("a")]);
        let t2 = MockTransport::new("stdio:s2", vec![mock_tool("b")]);
        supervisor.add_server(test_config("s1"), t1).await.unwrap();
        supervisor.add_server(test_config("s2"), t2).await.unwrap();

        supervisor.shutdown().await;

        let statuses = supervisor.server_statuses().await;
        assert!(statuses.is_empty());
    }

    #[test]
    fn backoff_increases_exponentially() {
        let config = test_config("test");
        let transport = MockTransport::new("test", vec![]);
        let mut server = SupervisedServer::new(config, transport);

        let d1 = server.next_backoff();
        let d2 = server.next_backoff();
        let d3 = server.next_backoff();

        // Each delay should roughly double (with jitter).
        assert!(d2 > d1 / 2); // accounting for jitter
        assert!(d3 > d2 / 2);
    }

    #[test]
    fn backoff_caps_at_max() {
        let config = test_config("test");
        let transport = MockTransport::new("test", vec![]);
        let mut server = SupervisedServer::new(config, transport);

        // Call next_backoff many times to hit the cap.
        for _ in 0..20 {
            server.next_backoff();
        }

        let delay = server.next_backoff();
        // Should be near MAX_BACKOFF (5 min) with jitter.
        assert!(delay <= MAX_BACKOFF + Duration::from_secs(76));
    }

    #[tokio::test]
    async fn health_check_detects_failure_and_transitions_to_backoff() {
        let (tx, mut rx) = mpsc::channel(10);
        let supervisor = Arc::new(McpSupervisor::new(tx, CancellationToken::new()));

        let transport = MockTransport::new("stdio:test", vec![mock_tool("ping")]);
        supervisor
            .add_server(
                test_config("test"),
                Arc::clone(&transport) as Arc<dyn McpTransport>,
            )
            .await
            .unwrap();
        let _ = rx.recv().await; // drain ServerConnected

        // Make transport fail.
        transport.set_fail(true);

        // Run a health check.
        supervisor.run_health_check().await;

        // Server should be in Backoff.
        let statuses = supervisor.server_statuses().await;
        assert_eq!(statuses[0].1, ServerState::Backoff);

        // Should have emitted ServerDisconnected.
        let event = rx.recv().await.unwrap();
        assert!(matches!(
            event,
            McpLifecycleEvent::ServerDisconnected { .. }
        ));
    }

    #[tokio::test]
    async fn health_check_reconnects_backoff_server() {
        let (tx, mut rx) = mpsc::channel(10);
        let supervisor = Arc::new(McpSupervisor::new(tx, CancellationToken::new()));

        let transport = MockTransport::new("stdio:test", vec![mock_tool("ping")]);
        supervisor
            .add_server(
                test_config("test"),
                Arc::clone(&transport) as Arc<dyn McpTransport>,
            )
            .await
            .unwrap();
        let _ = rx.recv().await; // drain ServerConnected

        // Make transport fail, then run health check to enter Backoff.
        transport.set_fail(true);
        supervisor.run_health_check().await;
        let _ = rx.recv().await; // drain ServerDisconnected

        // Now restore transport.
        transport.set_fail(false);

        // Run health check again — should reconnect.
        supervisor.run_health_check().await;

        let statuses = supervisor.server_statuses().await;
        assert_eq!(statuses[0].1, ServerState::Connected);

        // Should have emitted ServerReconnecting then ServerConnected.
        let event1 = rx.recv().await.unwrap();
        assert!(matches!(
            event1,
            McpLifecycleEvent::ServerReconnecting { .. }
        ));
        let event2 = rx.recv().await.unwrap();
        assert!(matches!(event2, McpLifecycleEvent::ServerConnected { .. }));
    }

    #[test]
    fn server_stats_record_call() {
        let mut stats = ServerStats::default();
        stats.record_call(100);
        assert_eq!(stats.total_calls, 1);
        assert!((stats.avg_latency_ms - 100.0).abs() < f64::EPSILON);

        stats.record_call(200);
        assert_eq!(stats.total_calls, 2);
        // EMA: 100 * 0.9 + 200 * 0.1 = 110
        assert!((stats.avg_latency_ms - 110.0).abs() < f64::EPSILON);
    }

    #[test]
    fn server_stats_record_failure() {
        let mut stats = ServerStats::default();
        stats.record_failure();
        stats.record_failure();
        assert_eq!(stats.failure_count, 2);

        stats.reset_failures();
        assert_eq!(stats.failure_count, 0);
        assert!(stats.connected_since.is_some());
    }

    #[test]
    fn server_state_display() {
        assert_eq!(ServerState::Connected.to_string(), "connected");
        assert_eq!(ServerState::Backoff.to_string(), "backoff");
        assert_eq!(ServerState::Stopped.to_string(), "stopped");
        assert_eq!(ServerState::Connecting.to_string(), "connecting");
    }
}
