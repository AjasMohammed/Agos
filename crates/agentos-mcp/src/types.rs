/// JSON-RPC 2.0 request/response types and MCP-specific message definitions.
///
/// Only the MCP methods used by AgentOS are modelled:
///   - `initialize` / `notifications/initialized`
///   - `tools/list`
///   - `tools/call`
use serde::{Deserialize, Serialize};

// ── JSON-RPC 2.0 primitives ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,       // always "2.0"
    pub id: serde_json::Value, // integer or string
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

// ── MCP-specific types ───────────────────────────────────────────────────────

/// An MCP tool definition as returned by `tools/list`.
///
/// Both `description` and `inputSchema` are optional per the MCP spec.
/// Missing values default to empty string / null respectively.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema object describing the tool's input parameters.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

// ── MCP Resource types ──────────────────────────────────────────────────────

/// An MCP resource definition as returned by `resources/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceDef {
    /// Unique URI identifying this resource (e.g. "agentos://agents", "agentos://tasks").
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// MIME type hint (e.g. "application/json").
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
}

/// Content returned when reading a resource via `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    /// The resource payload (typically JSON-serialised).
    pub text: String,
}

// ── MCP Prompt types ────────────────────────────────────────────────────────

/// An MCP prompt template as returned by `prompts/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Arguments the prompt accepts.
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

/// A single argument in a prompt template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

/// A message returned by `prompts/get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptMessage {
    pub role: String, // "user" or "assistant"
    pub content: McpPromptContent,
}

/// Content of a prompt message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptContent {
    #[serde(rename = "type")]
    pub content_type: String, // "text"
    pub text: String,
}

/// Server identity block returned in `initialize` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
}

/// Result payload for the `initialize` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: serde_json::Value,
    #[serde(rename = "serverInfo")]
    pub server_info: McpServerInfo,
}

// ── Constructors ─────────────────────────────────────────────────────────────

impl JsonRpcRequest {
    /// Build a request that carries a serializable params payload.
    pub fn new(id: u64, method: &str, params: impl Serialize) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: serde_json::Value::Number(id.into()),
            method: method.to_string(),
            params: Some(serde_json::to_value(params).unwrap_or(serde_json::Value::Null)),
        }
    }

    /// Build a request with no params (e.g. `tools/list`).
    pub fn new_no_params(id: u64, method: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: serde_json::Value::Number(id.into()),
            method: method.to_string(),
            params: None,
        }
    }
}

// ── Supervisor types ─────────────────────────────────────────────────────────

/// Lifecycle events emitted by the MCP supervisor for audit logging.
#[derive(Debug, Clone)]
pub enum McpLifecycleEvent {
    /// Server successfully connected and tools were discovered.
    ServerConnected { name: String, tool_count: usize },
    /// Server connection lost.
    ServerDisconnected { name: String, error: String },
    /// Attempting to reconnect to a server.
    ServerReconnecting { name: String, attempt: u32 },
    /// Server explicitly stopped (removed or shutdown).
    ServerStopped { name: String },
    /// A tool call completed (success or failure).
    ToolCallCompleted {
        server: String,
        tool: String,
        latency_ms: u64,
        success: bool,
    },
}

/// Connection state of a supervised MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    /// Transport is being established / initialize handshake in progress.
    Connecting,
    /// Server is alive and tools are registered.
    Connected,
    /// Connection failed; waiting for backoff timer before retrying.
    Backoff,
    /// Server was explicitly removed or shutdown. Terminal state.
    Stopped,
}

impl std::fmt::Display for ServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connecting => write!(f, "connecting"),
            Self::Connected => write!(f, "connected"),
            Self::Backoff => write!(f, "backoff"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// Runtime statistics for a supervised MCP server.
#[derive(Debug, Clone)]
pub struct ServerStats {
    /// When the server was first connected (or last reconnected).
    pub connected_since: Option<chrono::DateTime<chrono::Utc>>,
    /// Total number of tool calls made to this server.
    pub total_calls: u64,
    /// Number of consecutive connection failures.
    pub failure_count: u32,
    /// Running average latency of tool calls in milliseconds.
    pub avg_latency_ms: f64,
}

impl Default for ServerStats {
    fn default() -> Self {
        Self {
            connected_since: None,
            total_calls: 0,
            failure_count: 0,
            avg_latency_ms: 0.0,
        }
    }
}

impl ServerStats {
    /// Record a successful call with the given latency.
    pub fn record_call(&mut self, latency_ms: u64) {
        self.total_calls += 1;
        // Exponential moving average with alpha = 0.1
        if self.total_calls == 1 {
            self.avg_latency_ms = latency_ms as f64;
        } else {
            self.avg_latency_ms = self.avg_latency_ms * 0.9 + latency_ms as f64 * 0.1;
        }
    }

    /// Record a connection failure.
    pub fn record_failure(&mut self) {
        self.failure_count += 1;
    }

    /// Reset failure count on successful reconnect.
    pub fn reset_failures(&mut self) {
        self.failure_count = 0;
        self.connected_since = Some(chrono::Utc::now());
    }
}

// ── Constructors ─────────────────────────────────────────────────────────────

impl JsonRpcResponse {
    /// Convenience: build a successful response.
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Convenience: build an error response.
    pub fn err(id: serde_json::Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}
