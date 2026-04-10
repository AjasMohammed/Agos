use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::util::MAX_MCP_RESPONSE_BYTES;
use super::McpTransportError;
use crate::oauth::OAuthTokenProvider;
use crate::types::{JsonRpcRequest, JsonRpcResponse};

/// Default timeout for HTTP requests.
const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

/// Streamable HTTP transport for MCP servers (spec revision 2025-03-26).
///
/// Sends JSON-RPC requests as HTTP POST to a single endpoint. Accepts either
/// a direct JSON response (`application/json`) or an SSE upgrade
/// (`text/event-stream`) for long-running calls.
///
/// Supports two authentication modes:
/// - **Static Bearer token**: pass `auth_token` to `new()`.
/// - **OAuth2**: pass an `OAuthTokenProvider` to `new_with_oauth()`. The provider
///   is called on every request and handles transparent token refresh. On a 401
///   response the transport forces a refresh and retries the request once.
pub struct StreamableHttpTransport {
    /// Display name for logging.
    name: String,
    /// The MCP server endpoint URL (e.g. "https://mcp-server.zomato.com/mcp").
    url: String,
    /// Optional static Bearer token for authentication.
    auth_token: Option<String>,
    /// Optional OAuth2 token provider (mutually exclusive with `auth_token`).
    oauth_provider: Option<Arc<dyn OAuthTokenProvider>>,
    /// Per-request timeout.
    timeout: Duration,
    /// Reqwest HTTP client (connection pooling built-in).
    client: reqwest::Client,
}

impl StreamableHttpTransport {
    /// Create a new HTTP transport with a static Bearer token (or no auth).
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
            oauth_provider: None,
            timeout,
            client,
        })
    }

    /// Create a new HTTP transport with OAuth2 token provider.
    ///
    /// The provider is called before every request. On HTTP 401 the transport
    /// calls `force_refresh()` on the provider and retries the request once.
    pub fn new_with_oauth(
        name: String,
        url: String,
        oauth_provider: Arc<dyn OAuthTokenProvider>,
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
            auth_token: None,
            oauth_provider: Some(oauth_provider),
            timeout,
            client,
        })
    }

    /// Resolve the Bearer token to use for this request.
    ///
    /// Returns `None` if neither a static token nor an OAuth provider is configured.
    async fn resolve_token(&self) -> Result<Option<String>, McpTransportError> {
        if let Some(ref provider) = self.oauth_provider {
            let token = provider.get_token().await?;
            return Ok(Some(token));
        }
        Ok(self.auth_token.clone())
    }

    /// Execute a single POST to the MCP endpoint with the given Bearer token.
    async fn execute_post(
        &self,
        req: &JsonRpcRequest,
        token: Option<String>,
    ) -> Result<JsonRpcResponse, McpTransportError> {
        let mut request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");

        if let Some(ref t) = token {
            request = request.header("Authorization", format!("Bearer {}", t));
        }

        let http_resp = request.json(req).send().await.map_err(|e| {
            if e.is_timeout() {
                McpTransportError::Timeout(self.timeout)
            } else if e.is_connect() {
                McpTransportError::Connection(format!("HTTP connection refused: {}", e))
            } else {
                McpTransportError::Connection(format!("HTTP request failed: {}", e))
            }
        })?;

        let status = http_resp.status();

        // Surface 401 as a distinct Auth error so the caller can refresh and retry.
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let body = http_resp.text().await.unwrap_or_default();
            return Err(McpTransportError::Auth(format!(
                "HTTP 401 Unauthorized: {}",
                body
            )));
        }

        if !status.is_success() {
            let body = http_resp.text().await.unwrap_or_default();
            return Err(McpTransportError::Protocol {
                code: -(status.as_u16() as i64),
                message: format!("HTTP {}: {}", status, body),
            });
        }

        // Reject oversized responses before reading the body into memory.
        if let Some(content_length) = http_resp.content_length() {
            if content_length > MAX_MCP_RESPONSE_BYTES as u64 {
                return Err(McpTransportError::Connection(format!(
                    "HTTP response too large: {} bytes (limit: {} bytes)",
                    content_length, MAX_MCP_RESPONSE_BYTES
                )));
            }
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

        // Post-read size check for chunked responses (no Content-Length header).
        if body.len() > MAX_MCP_RESPONSE_BYTES {
            return Err(McpTransportError::Connection(format!(
                "HTTP response body too large: {} bytes (limit: {} bytes)",
                body.len(),
                MAX_MCP_RESPONSE_BYTES
            )));
        }

        if content_type.contains("text/event-stream") {
            let resp = self.parse_sse_response(&body).await?;
            if let Some(ref err) = resp.error {
                return Err(McpTransportError::Protocol {
                    code: err.code,
                    message: err.message.clone(),
                });
            }
            Ok(resp)
        } else {
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
        let token = self.resolve_token().await?;

        match self.execute_post(req, token).await {
            Ok(resp) => Ok(resp),

            // On 401: force-refresh the OAuth token and retry once.
            Err(McpTransportError::Auth(msg)) => {
                if let Some(ref provider) = self.oauth_provider {
                    tracing::warn!(
                        transport = %self.name,
                        "HTTP 401 received — forcing OAuth token refresh and retrying"
                    );
                    let new_token = provider.force_refresh().await.map_err(|e| {
                        McpTransportError::Auth(format!("Token refresh after 401 failed: {}", e))
                    })?;
                    // Retry with the refreshed token (do not retry again on second 401).
                    self.execute_post(req, Some(new_token)).await
                } else {
                    // Static token — cannot refresh; propagate the auth error.
                    Err(McpTransportError::Auth(msg))
                }
            }

            Err(e) => Err(e),
        }
    }

    async fn send_notification(&self, req: &JsonRpcRequest) -> Result<(), McpTransportError> {
        let token = self.resolve_token().await?;

        let mut request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json");

        if let Some(ref t) = token {
            request = request.header("Authorization", format!("Bearer {}", t));
        }

        // Fire-and-forget: send the notification, ignore the response body.
        let resp = request.json(req).send().await.map_err(|e| {
            if e.is_connect() {
                McpTransportError::Connection(format!("HTTP connection refused: {}", e))
            } else {
                McpTransportError::Connection(format!("HTTP notification failed: {}", e))
            }
        })?;

        // On 401, trigger a proactive refresh so the next send() call uses a fresh token.
        // Notifications are fire-and-forget so we do not retry the notification itself.
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            if let Some(ref provider) = self.oauth_provider {
                tracing::warn!(
                    transport = %self.name,
                    "HTTP 401 on notification — triggering background token refresh"
                );
                let _ = provider.force_refresh().await;
            }
        }

        Ok(())
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        // HTTP transport is stateless — nothing to close.
        Ok(())
    }

    fn transport_name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::McpTransport;

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
        assert_eq!(
            transport.timeout,
            Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS)
        );
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
        let transport =
            StreamableHttpTransport::new("test".into(), "http://localhost/mcp".into(), None, None)
                .unwrap();

        let sse_body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let resp = transport.parse_sse_response(sse_body).await.unwrap();
        assert_eq!(resp.id, serde_json::json!(1));
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn parse_sse_response_no_data_returns_error() {
        let transport =
            StreamableHttpTransport::new("test".into(), "http://localhost/mcp".into(), None, None)
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

    #[test]
    fn auth_error_does_not_reconnect() {
        let err = McpTransportError::Auth("401 Unauthorized".into());
        assert!(!err.should_reconnect());
        assert!(err.is_auth_error());
    }
}
