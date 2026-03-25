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
//!   (e.g. Claude Desktop, Cursor) via the `agentctl mcp serve` subcommand.
//!
//! # Security
//!
//! MCP tools imported via `McpToolAdapter` are subject to the same
//! `PermissionSet` and capability-token checks as native tools. The adapter
//! does not bypass AgentOS security — it is a protocol bridge, not a bypass.

pub mod adapter;
pub mod client;
pub mod handle;
pub mod server;
pub mod types;

pub use adapter::McpToolAdapter;
pub use client::McpClient;
pub use handle::McpServerHandle;
pub use server::{McpServer, McpToolExecutor};
pub use types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpToolDef};
