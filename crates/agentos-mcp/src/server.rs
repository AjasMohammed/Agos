/// MCP server — expose AgentOS tools, resources, and prompts to external
/// MCP clients via stdio or HTTP.
///
/// This enables tools like Claude Desktop, Cursor, or any other MCP-capable
/// client to invoke AgentOS tools using the standard protocol.
///
/// Usage:
/// ```ignore
/// agentos mcp serve                      # stdio (local, no auth)
/// agentos mcp serve --transport http      # HTTP with CapToken auth
/// ```
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncWriteExt, BufReader};

use crate::transport::util::{read_line_limited, MAX_MCP_RESPONSE_BYTES};
use crate::types::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpPromptDef, McpPromptMessage,
    McpResourceContent, McpResourceDef, McpToolDef,
};

// ── Executor trait ────────────────────────────────────────────────────────────

/// Abstraction over the AgentOS kernel used inside `McpServer`.
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

    /// Return all available resources as MCP resource definitions.
    async fn list_resources(&self) -> Vec<McpResourceDef> {
        vec![] // default: no resources
    }

    /// Read a resource by URI. Returns content or an error string.
    async fn read_resource(&self, _uri: &str) -> Result<McpResourceContent, String> {
        Err("Resources not supported".to_string())
    }

    /// Return all available prompts as MCP prompt definitions.
    async fn list_prompts(&self) -> Vec<McpPromptDef> {
        vec![] // default: no prompts
    }

    /// Get a prompt by name with the given arguments.
    async fn get_prompt(
        &self,
        _name: &str,
        _args: serde_json::Value,
    ) -> Result<Vec<McpPromptMessage>, String> {
        Err("Prompts not supported".to_string())
    }

    /// Perform LLM sampling (inference) on behalf of the MCP client.
    ///
    /// The default implementation returns an error — kernels that expose LLM
    /// access should override this.  Implementations must enforce cost budgets
    /// before dispatching to the LLM.
    async fn create_message(
        &self,
        _messages: serde_json::Value,
        _model_preferences: Option<serde_json::Value>,
        _max_tokens: Option<u64>,
    ) -> Result<serde_json::Value, String> {
        Err("Sampling not supported by this server".to_string())
    }
}

// ── Auth ─────────────────────────────────────────────────────────────────────

/// Token validator for MCP server authentication.
///
/// Stdio transport skips auth (local-only). HTTP transport requires a
/// Bearer token validated against this trait.
#[async_trait]
pub trait McpAuthValidator: Send + Sync {
    /// Validate a bearer token string.  Returns `Ok(())` if the token is
    /// valid and grants MCP access, or an error message if rejected.
    async fn validate_token(&self, token: &str) -> Result<(), String>;
}

/// No-op authenticator — always allows (used for stdio transport).
pub struct NoAuth;

#[async_trait]
impl McpAuthValidator for NoAuth {
    async fn validate_token(&self, _token: &str) -> Result<(), String> {
        Ok(())
    }
}

// ── McpServer ─────────────────────────────────────────────────────────────────

/// Serves AgentOS tools, resources, and prompts as an MCP server.
pub struct McpServer {
    executor: Arc<dyn McpToolExecutor>,
    auth: Arc<dyn McpAuthValidator>,
}

impl McpServer {
    /// Create a server with no authentication (for stdio transport).
    pub fn new(executor: Arc<dyn McpToolExecutor>) -> Self {
        Self {
            executor,
            auth: Arc::new(NoAuth),
        }
    }

    /// Create a server with a token authenticator (for HTTP transport).
    pub fn with_auth(executor: Arc<dyn McpToolExecutor>, auth: Arc<dyn McpAuthValidator>) -> Self {
        Self { executor, auth }
    }

    /// Validate a bearer token. Returns an error response if invalid.
    pub async fn authenticate(&self, token: Option<&str>) -> Result<(), JsonRpcResponse> {
        if let Some(t) = token {
            self.auth.validate_token(t).await.map_err(|e| {
                JsonRpcResponse::err(
                    serde_json::Value::Null,
                    -32000,
                    format!("Unauthorized: {}", e),
                )
            })
        } else {
            // No token provided — only allowed for NoAuth (stdio)
            self.auth.validate_token("").await.map_err(|e| {
                JsonRpcResponse::err(
                    serde_json::Value::Null,
                    -32000,
                    format!("Unauthorized: {}", e),
                )
            })
        }
    }

    /// Run the MCP server loop, reading JSON-RPC requests from stdin and
    /// writing responses to stdout.  Runs until stdin is closed (EOF).
    pub async fn serve_stdio(&self) -> anyhow::Result<()> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut writer = tokio::io::BufWriter::new(stdout);

        loop {
            let mut line = String::new();
            let n = read_line_limited(&mut reader, &mut line, MAX_MCP_RESPONSE_BYTES).await?;
            if n == 0 {
                break; // EOF
            }
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
            // ── Initialize handshake ────────────────────────────────────
            "initialize" => JsonRpcResponse::ok(
                req.id,
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {},
                        "resources": {},
                        "prompts": {},
                        "sampling": {}
                    },
                    "serverInfo": {
                        "name": "agentos",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ),

            // ── Tools ───────────────────────────────────────────────────
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

            // ── Resources ───────────────────────────────────────────────
            "resources/list" => {
                let resources = self.executor.list_resources().await;
                JsonRpcResponse::ok(req.id, serde_json::json!({ "resources": resources }))
            }

            "resources/read" => {
                let uri = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("uri"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if uri.is_empty() {
                    return JsonRpcResponse::err(req.id, -32602, "Missing 'uri' in params");
                }
                match self.executor.read_resource(&uri).await {
                    Ok(content) => {
                        JsonRpcResponse::ok(req.id, serde_json::json!({ "contents": [content] }))
                    }
                    Err(e) => JsonRpcResponse::err(req.id, -32603, e),
                }
            }

            // ── Prompts ─────────────────────────────────────────────────
            "prompts/list" => {
                let prompts = self.executor.list_prompts().await;
                JsonRpcResponse::ok(req.id, serde_json::json!({ "prompts": prompts }))
            }

            "prompts/get" => {
                let name = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    return JsonRpcResponse::err(req.id, -32602, "Missing 'name' in params");
                }
                let args = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                match self.executor.get_prompt(&name, args).await {
                    Ok(messages) => {
                        JsonRpcResponse::ok(req.id, serde_json::json!({ "messages": messages }))
                    }
                    Err(e) => JsonRpcResponse::err(req.id, -32603, e),
                }
            }

            // ── Sampling ────────────────────────────────────────────────
            "sampling/createMessage" => {
                let params = req.params.as_ref().cloned().unwrap_or_default();
                let messages = params
                    .get("messages")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(vec![]));
                let model_prefs = params.get("modelPreferences").cloned();
                let max_tokens = params.get("maxTokens").and_then(|v| v.as_u64());

                match self
                    .executor
                    .create_message(messages, model_prefs, max_tokens)
                    .await
                {
                    Ok(result) => JsonRpcResponse::ok(req.id, result),
                    Err(e) => JsonRpcResponse::err(req.id, -32603, e),
                }
            }

            // ── Unknown ─────────────────────────────────────────────────
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

        async fn list_resources(&self) -> Vec<McpResourceDef> {
            vec![McpResourceDef {
                uri: "agentos://agents".to_string(),
                name: "agents".to_string(),
                description: "List of registered agents".to_string(),
                mime_type: "application/json".to_string(),
            }]
        }

        async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, String> {
            if uri == "agentos://agents" {
                Ok(McpResourceContent {
                    uri: uri.to_string(),
                    mime_type: "application/json".to_string(),
                    text: r#"[{"name":"demo-agent","status":"idle"}]"#.to_string(),
                })
            } else {
                Err(format!("Resource not found: {}", uri))
            }
        }

        async fn list_prompts(&self) -> Vec<McpPromptDef> {
            vec![McpPromptDef {
                name: "research".to_string(),
                description: "Research a topic".to_string(),
                arguments: vec![McpPromptArgument {
                    name: "topic".to_string(),
                    description: "The topic to research".to_string(),
                    required: true,
                }],
            }]
        }

        async fn get_prompt(
            &self,
            name: &str,
            args: serde_json::Value,
        ) -> Result<Vec<McpPromptMessage>, String> {
            if name == "research" {
                let topic = args
                    .get("topic")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                Ok(vec![McpPromptMessage {
                    role: "user".to_string(),
                    content: McpPromptContent {
                        content_type: "text".to_string(),
                        text: format!("Research the following topic: {}", topic),
                    },
                }])
            } else {
                Err(format!("Prompt not found: {}", name))
            }
        }
    }

    use crate::types::{
        McpPromptArgument, McpPromptContent, McpPromptDef, McpPromptMessage, McpResourceContent,
        McpResourceDef,
    };

    // ── Auth tests ──────────────────────────────────────────────────────
    struct RejectAuth;
    #[async_trait]
    impl McpAuthValidator for RejectAuth {
        async fn validate_token(&self, _token: &str) -> Result<(), String> {
            Err("Invalid token".to_string())
        }
    }

    #[tokio::test]
    async fn test_auth_rejects_invalid_token() {
        let server = McpServer::with_auth(MockExecutor::empty(), Arc::new(RejectAuth));
        let result = server.authenticate(Some("bad-token")).await;
        assert!(result.is_err());
        let resp = result.unwrap_err();
        assert!(resp.error.is_some());
        assert!(resp.error.unwrap().message.contains("Unauthorized"));
    }

    #[tokio::test]
    async fn test_noauth_allows_all() {
        let server = McpServer::new(MockExecutor::empty());
        assert!(server.authenticate(Some("anything")).await.is_ok());
        assert!(server.authenticate(None).await.is_ok());
    }

    // ── Initialize ──────────────────────────────────────────────────────
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
    async fn test_initialize_advertises_resources_and_prompts() {
        let server = McpServer::new(MockExecutor::empty());
        let req = JsonRpcRequest::new_no_params(1, "initialize");
        let resp = server.handle_request(req).await;
        let caps = &resp.result.unwrap()["capabilities"];
        assert!(caps.get("tools").is_some());
        assert!(caps.get("resources").is_some());
        assert!(caps.get("prompts").is_some());
    }

    // ── Tools ───────────────────────────────────────────────────────────
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

    // ── Resources ───────────────────────────────────────────────────────
    #[tokio::test]
    async fn test_resources_list() {
        let server = McpServer::new(MockExecutor::with_tools(&[]));
        let req = JsonRpcRequest::new_no_params(5, "resources/list");
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let resources = resp.result.unwrap()["resources"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["uri"], "agentos://agents");
    }

    #[tokio::test]
    async fn test_resources_read_success() {
        let server = McpServer::new(MockExecutor::with_tools(&[]));
        let req = JsonRpcRequest::new(6, "resources/read", json!({"uri": "agentos://agents"}));
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let contents = resp.result.unwrap()["contents"].as_array().unwrap().clone();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["uri"], "agentos://agents");
    }

    #[tokio::test]
    async fn test_resources_read_not_found() {
        let server = McpServer::new(MockExecutor::with_tools(&[]));
        let req = JsonRpcRequest::new(7, "resources/read", json!({"uri": "agentos://missing"}));
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn test_resources_read_missing_uri() {
        let server = McpServer::new(MockExecutor::with_tools(&[]));
        let req = JsonRpcRequest::new(8, "resources/read", json!({}));
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    // ── Prompts ─────────────────────────────────────────────────────────
    #[tokio::test]
    async fn test_prompts_list() {
        let server = McpServer::new(MockExecutor::with_tools(&[]));
        let req = JsonRpcRequest::new_no_params(9, "prompts/list");
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let prompts = resp.result.unwrap()["prompts"].as_array().unwrap().clone();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["name"], "research");
    }

    #[tokio::test]
    async fn test_prompts_get_success() {
        let server = McpServer::new(MockExecutor::with_tools(&[]));
        let req = JsonRpcRequest::new(
            10,
            "prompts/get",
            json!({"name": "research", "arguments": {"topic": "AI safety"}}),
        );
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let messages = resp.result.unwrap()["messages"].as_array().unwrap().clone();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let text = messages[0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("AI safety"));
    }

    #[tokio::test]
    async fn test_prompts_get_not_found() {
        let server = McpServer::new(MockExecutor::with_tools(&[]));
        let req = JsonRpcRequest::new(11, "prompts/get", json!({"name": "nonexistent"}));
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
    }

    // ── Unknown method ──────────────────────────────────────────────────
    #[tokio::test]
    async fn test_unknown_method_returns_method_not_found() {
        let server = McpServer::new(MockExecutor::empty());
        let req = JsonRpcRequest::new_no_params(99, "not/a/method");
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }
}
