/// MCP server — expose AgentOS tools to external MCP clients via stdio.
///
/// This enables tools like Claude Desktop, Cursor, or any other MCP-capable
/// client to invoke AgentOS tools using the standard protocol.
///
/// Usage:
/// ```ignore
/// agentctl mcp serve   # reads stdin, writes stdout
/// ```
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpToolDef};

// ── Executor trait ────────────────────────────────────────────────────────────

/// Abstraction over the AgentOS `ToolRunner` used inside `McpServer`.
///
/// This thin trait is the seam between the MCP protocol layer and the kernel's
/// tool execution engine. In production, the kernel provides a concrete
/// implementation backed by `ToolRunner`. In tests, a `MockMcpExecutor` is used.
#[async_trait]
pub trait McpToolExecutor: Send + Sync {
    /// Return all available tools as MCP tool definitions.
    async fn list_tools(&self) -> Vec<McpToolDef>;

    /// Execute a tool by name with the given JSON arguments.
    ///
    /// Returns the serialised result on success or an error string on failure.
    async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String>;
}

// ── McpServer ─────────────────────────────────────────────────────────────────

/// Serves AgentOS tools as an MCP server over stdin/stdout.
pub struct McpServer {
    executor: Arc<dyn McpToolExecutor>,
}

impl McpServer {
    pub fn new(executor: Arc<dyn McpToolExecutor>) -> Self {
        Self { executor }
    }

    /// Run the MCP server loop, reading JSON-RPC requests from stdin and
    /// writing responses to stdout.  Runs until stdin is closed (EOF).
    pub async fn serve_stdio(&self) -> anyhow::Result<()> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin).lines();
        let mut writer = tokio::io::BufWriter::new(stdout);

        while let Some(line) = reader.next_line().await? {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Parse once as a generic JSON value. On failure, send a parse-error response.
            let value: serde_json::Value = match serde_json::from_str::<serde_json::Value>(line) {
                Ok(v) => v,
                Err(e) => {
                    let resp = JsonRpcResponse::err(
                        serde_json::Value::Null,
                        -32700,
                        format!("Parse error: {}", e),
                    );
                    let mut s = serde_json::to_string(&resp)?;
                    s.push('\n');
                    writer.write_all(s.as_bytes()).await?;
                    writer.flush().await?;
                    continue;
                }
            };

            // Skip pure notifications (no `id` field) — they don't require a response.
            if value.get("id").is_none() {
                continue;
            }

            // Convert the already-parsed value into a typed request (no second parse).
            let resp = match serde_json::from_value::<JsonRpcRequest>(value) {
                Ok(req) => self.handle_request(req).await,
                Err(e) => JsonRpcResponse::err(
                    serde_json::Value::Null,
                    -32700,
                    format!("Parse error: {}", e),
                ),
            };

            let mut s = serde_json::to_string(&resp)?;
            s.push('\n');
            writer.write_all(s.as_bytes()).await?;
            writer.flush().await?;
        }

        Ok(())
    }

    /// Dispatch a single JSON-RPC request and produce a response.
    ///
    /// This method is `pub` so it can be exercised directly in unit tests
    /// without needing to wire up stdio.
    pub async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => JsonRpcResponse::ok(
                req.id,
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "agentos",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ),

            "tools/list" => {
                let tools = self.executor.list_tools().await;
                JsonRpcResponse::ok(req.id, serde_json::json!({ "tools": tools }))
            }

            "tools/call" => {
                let (name, args) = extract_tool_call_params(req.params.as_ref());
                if name.is_empty() {
                    return JsonRpcResponse::err(req.id, -32602, "Missing 'name' in params");
                }
                match self.executor.call_tool(&name, args).await {
                    Ok(result) => JsonRpcResponse::ok(
                        req.id,
                        serde_json::json!({
                            "content": [{ "type": "text", "text": result.to_string() }]
                        }),
                    ),
                    Err(e) => JsonRpcResponse::err(req.id, -32603, e),
                }
            }

            other => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", other),
                    data: None,
                }),
            },
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn extract_tool_call_params(params: Option<&serde_json::Value>) -> (String, serde_json::Value) {
    match params {
        Some(p) => {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = p
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));
            (name, args)
        }
        None => (String::new(), serde_json::Value::Object(Default::default())),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct MockExecutor {
        tools: Vec<McpToolDef>,
    }

    impl MockExecutor {
        fn with_tools(names: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                tools: names
                    .iter()
                    .map(|n| McpToolDef {
                        name: n.to_string(),
                        description: format!("Mock tool {}", n),
                        input_schema: json!({"type": "object"}),
                    })
                    .collect(),
            })
        }
        fn empty() -> Arc<Self> {
            Arc::new(Self { tools: vec![] })
        }
    }

    #[async_trait]
    impl McpToolExecutor for MockExecutor {
        async fn list_tools(&self) -> Vec<McpToolDef> {
            self.tools.clone()
        }
        async fn call_tool(
            &self,
            name: &str,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            if self.tools.iter().any(|t| t.name == name) {
                Ok(json!({"ok": true}))
            } else {
                Err(format!("Tool '{}' not found", name))
            }
        }
    }

    #[tokio::test]
    async fn test_initialize_returns_server_info() {
        let server = McpServer::new(MockExecutor::empty());
        let req = JsonRpcRequest::new_no_params(1, "initialize");
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["serverInfo"]["name"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_tools_list_returns_registered_tools() {
        let server = McpServer::new(MockExecutor::with_tools(&["ping", "echo"]));
        let req = JsonRpcRequest::new_no_params(2, "tools/list");
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "ping");
    }

    #[tokio::test]
    async fn test_tools_call_success() {
        let server = McpServer::new(MockExecutor::with_tools(&["ping"]));
        let req = JsonRpcRequest::new(3, "tools/call", json!({"name": "ping", "arguments": {}}));
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_tools_call_unknown_tool_returns_error() {
        let server = McpServer::new(MockExecutor::empty());
        let req = JsonRpcRequest::new(
            4,
            "tools/call",
            json!({"name": "nonexistent", "arguments": {}}),
        );
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32603);
    }

    #[tokio::test]
    async fn test_unknown_method_returns_method_not_found() {
        let server = McpServer::new(MockExecutor::empty());
        let req = JsonRpcRequest::new_no_params(99, "not/a/method");
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }
}
