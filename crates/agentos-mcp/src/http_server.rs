/// MCP HTTP server — serve AgentOS tools via Streamable HTTP transport.
///
/// Exposes the same MCP JSON-RPC interface as the stdio server, but over
/// HTTP POST with optional Bearer token authentication.
///
/// Endpoints:
///   POST /mcp          — Submit a JSON-RPC request; returns JSON or SSE stream
///   GET  /mcp/health   — Health check (returns 200 OK)
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

/// Maximum MCP request body size (256 KiB). Matches agentos-web's CSP policy.
const MAX_BODY_BYTES: usize = 256 * 1024;

use crate::server::{McpAuthValidator, McpServer, McpToolExecutor};
use crate::types::JsonRpcRequest;

// ── App state ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct McpHttpState {
    server: Arc<McpServer>,
}

// ── Router ────────────────────────────────────────────────────────────────────

/// Build an Axum router for the MCP HTTP server.
pub fn build_router(executor: Arc<dyn McpToolExecutor>, auth: Arc<dyn McpAuthValidator>) -> Router {
    let state = McpHttpState {
        server: Arc::new(McpServer::with_auth(executor, auth)),
    };

    Router::new()
        .route("/mcp", post(handle_mcp_request))
        .route("/mcp/health", get(handle_health))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// Serve the MCP HTTP server on the given address until shutdown.
pub async fn serve(
    executor: Arc<dyn McpToolExecutor>,
    auth: Arc<dyn McpAuthValidator>,
    bind_addr: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let app = build_router(executor, auth);

    tracing::info!(%bind_addr, "MCP HTTP server listening");

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn handle_health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn handle_mcp_request(
    State(state): State<McpHttpState>,
    headers: HeaderMap,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    // Extract Bearer token from Authorization header
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.to_string());

    // Validate auth
    if let Err(err_resp) = state.server.authenticate(token.as_deref()).await {
        let body = serde_json::to_string(&err_resp).unwrap_or_default();
        return (
            StatusCode::UNAUTHORIZED,
            [("content-type", "application/json")],
            body,
        )
            .into_response();
    }

    // Dispatch to MCP handler
    let resp = state.server.handle_request(req).await;
    let body = serde_json::to_string(&resp).unwrap_or_default();
    (StatusCode::OK, [("content-type", "application/json")], body).into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{McpToolExecutor, NoAuth};
    use crate::types::{JsonRpcRequest, McpToolDef};
    use axum::body::Body;
    use axum::http::{Method, Request as HttpRequest};
    use tower::util::ServiceExt;

    struct MockExec;

    #[async_trait::async_trait]
    impl McpToolExecutor for MockExec {
        async fn list_tools(&self) -> Vec<McpToolDef> {
            vec![McpToolDef {
                name: "ping".to_string(),
                description: "Ping tool".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }]
        }
        async fn call_tool(
            &self,
            _: &str,
            _: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({"pong": true}))
        }
    }

    fn make_app() -> Router<()> {
        build_router(Arc::new(MockExec), Arc::new(NoAuth))
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let app = make_app();
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("/mcp/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_tools_list_via_http() {
        let app = make_app();
        let body = serde_json::to_string(&JsonRpcRequest::new_no_params(1, "tools/list")).unwrap();
        let req = HttpRequest::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["result"]["tools"].as_array().is_some());
    }

    #[tokio::test]
    async fn test_auth_rejection() {
        struct RejectAll;
        #[async_trait::async_trait]
        impl McpAuthValidator for RejectAll {
            async fn validate_token(&self, _: &str) -> Result<(), String> {
                Err("forbidden".to_string())
            }
        }

        let app = build_router(Arc::new(MockExec), Arc::new(RejectAll));
        let body = serde_json::to_string(&JsonRpcRequest::new_no_params(1, "tools/list")).unwrap();
        let req = HttpRequest::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_bearer_token_accepted() {
        struct AcceptFoo;
        #[async_trait::async_trait]
        impl McpAuthValidator for AcceptFoo {
            async fn validate_token(&self, token: &str) -> Result<(), String> {
                if token == "foo" {
                    Ok(())
                } else {
                    Err("bad token".into())
                }
            }
        }

        let app = build_router(Arc::new(MockExec), Arc::new(AcceptFoo));
        let body = serde_json::to_string(&JsonRpcRequest::new_no_params(1, "tools/list")).unwrap();
        let req = HttpRequest::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer foo")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
