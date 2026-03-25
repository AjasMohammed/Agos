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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the tool's input parameters.
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
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
