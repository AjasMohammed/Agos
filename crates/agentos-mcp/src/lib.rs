//! AgentOS MCP Adapter
//!
//! Implements the [Model Context Protocol](https://modelcontextprotocol.io/) (MCP)
//! for AgentOS, enabling:
//!
//! - **Client mode**: Import tools from any MCP-compliant server. Each imported
//!   tool is wrapped as an [`AgentTool`] with `TrustTier::Community` and goes
//!   through standard AgentOS capability-token enforcement.
//!
//! - **Server mode**: Expose registered AgentOS tools to external MCP clients
//!   (e.g. Claude Desktop, Cursor) via the `agentos mcp serve` subcommand.
//!
//! # Security
//!
//! MCP tools imported via `McpToolAdapter` are subject to the same
//! `PermissionSet` and capability-token checks as native tools. The adapter
//! does not bypass AgentOS security — it is a protocol bridge, not a bypass.

pub mod a2a;
pub mod adapter;
pub mod client;
pub mod handle;
pub mod http_server;
pub mod oauth;
pub mod security;
pub mod server;
pub mod supervisor;
pub mod transport;
pub mod types;

pub use a2a::server::A2ATaskExecutor;
pub use a2a::{
    build_a2a_router, A2AClient, A2ATask, A2ATaskStatus, AgentCapability, AgentCard,
    AuthRequirement,
};
pub use adapter::McpToolAdapter;
pub use client::McpClient;
pub use handle::McpServerHandle;
pub use http_server::{build_router as build_http_router, serve as serve_http};
pub use oauth::OAuthTokenProvider;
pub use security::{McpSecurityGate, McpServerPolicy, SlidingWindowRateLimiter};
pub use server::{McpAuthValidator, McpServer, McpToolExecutor, NoAuth};
pub use supervisor::{McpServerResolvedConfig, McpSupervisor, SupervisedServer};
pub use transport::{McpTransport, McpTransportError, McpTransportFactory};
pub use types::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpLifecycleEvent, McpPromptArgument,
    McpPromptContent, McpPromptDef, McpPromptMessage, McpResourceContent, McpResourceDef,
    McpToolDef, ServerState, ServerStats,
};
