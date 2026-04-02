/// Kernel handler for `KernelCommand::McpStatus`.
///
/// Queries the MCP supervisor for live server state and converts to
/// the bus-compatible `McpServerStatus` format.
use agentos_bus::{KernelResponse, McpServerStatus};

use crate::kernel::Kernel;

impl Kernel {
    /// Return the live health status of all configured MCP server connections.
    pub async fn cmd_mcp_status(&self) -> KernelResponse {
        let statuses: Vec<McpServerStatus> = self
            .mcp_supervisor
            .server_statuses()
            .await
            .into_iter()
            .map(
                |(name, state, tool_count, _stats, backoff_msg)| McpServerStatus {
                    name,
                    connected: state == agentos_mcp::ServerState::Connected,
                    tool_count,
                    last_error: backoff_msg,
                },
            )
            .collect();

        KernelResponse::McpServerStatusList(statuses)
    }
}
